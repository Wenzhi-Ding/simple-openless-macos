//! RapidOCR worker 常驻进程托管（全同步：屏幕 OCR 的观测线程不走 tokio）。
//!
//! worker 首次使用时惰性拉起（Python 冷启动 + 引擎初始化约一两秒，由观测的
//! 5 秒截止吸收），之后常驻。协议与 qwen3_worker.py 相同：stdin/stdout 上
//! 换行分隔 JSON；stdout 由专用 reader 线程推进 mpsc；请求路径是「加锁 →
//! 写一行 → 按 id 等响应行」，因此在途请求至多一个，响应不会交错。
//!
//! 失败即毒化：写失败、响应超时、worker 退出之后，engine 不再尝试复活
//! （避免每次听写都重新拉起一个坏进程），普通日志只记状态不记图片内容。

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use base64::Engine as _;
use serde::Deserialize;

/// worker 启动 + 引擎初始化的上限（Python 冷启动 + RapidOCR 加载本地 ONNX）。
const BOOT_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Deserialize)]
struct WorkerLine {
    id: Option<u64>,
    ok: Option<bool>,
    text: Option<String>,
    error: Option<String>,
    event: Option<String>,
}

struct OcrWorker {
    _child: Child,
    stdin: ChildStdin,
    lines: mpsc::Receiver<WorkerLine>,
    poisoned: bool,
}

pub(crate) struct OcrEngine {
    worker: Mutex<OcrWorker>,
    next_id: AtomicU64,
}

impl OcrEngine {
    fn spawn() -> Result<OcrEngine> {
        let (python, script) = crate::asr::local::qwen3_mlx::resolve_ocr_worker_locations()
            .context("the bundled OCR runtime is missing; reinstall the app")?;
        let mut command = Command::new(&python);
        command
            .arg("-I")
            .arg("-B")
            .arg(&script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("HF_HUB_OFFLINE", "1")
            .env("TRANSFORMERS_OFFLINE", "1")
            .env("PYTHONUNBUFFERED", "1");
        // 隔离宿主可能注入的 Python 配置，保证行为可复现（与 qwen3 worker 同）。
        for key in [
            "PYTHONPATH",
            "PYTHONHOME",
            "PYTHONSTARTUP",
            "PYTHONUSERBASE",
        ] {
            command.env_remove(key);
        }
        let mut child = command
            .spawn()
            .with_context(|| format!("spawn OCR worker failed: {}", python.display()))?;
        let stdin = child.stdin.take().context("worker stdin was not piped")?;
        let stdout = child.stdout.take().context("worker stdout was not piped")?;
        let stderr = child.stderr.take().context("worker stderr was not piped")?;

        let (tx, rx) = mpsc::channel::<WorkerLine>();
        std::thread::Builder::new()
            .name("openless-ocr-worker-stdout".into())
            .spawn(move || {
                let reader = BufReader::new(stdout);
                for line in reader.lines() {
                    let Ok(line) = line else { break };
                    if line.trim().is_empty() {
                        continue;
                    }
                    match serde_json::from_str::<WorkerLine>(&line) {
                        Ok(parsed) => {
                            if tx.send(parsed).is_err() {
                                return;
                            }
                        }
                        // worker 协议外输出只允许出现在崩溃路径；记字节数不记内容。
                        Err(_) => {
                            log::warn!("[ocr-worker] unparsable stdout line ({} bytes)", line.len())
                        }
                    }
                }
                // worker 退出：tx 随线程结束 drop，请求侧的 recv 以 Disconnected 感知。
            })
            .context("spawn ocr worker stdout reader failed")?;

        // stderr 只可能来 Python 级崩溃；透传进普通日志（异常文本不含截图内容）。
        std::thread::Builder::new()
            .name("openless-ocr-worker-stderr".into())
            .spawn(move || {
                let reader = BufReader::new(stderr);
                for line in reader.lines() {
                    let Ok(line) = line else { break };
                    log::warn!("[ocr-worker] stderr: {line}");
                }
            })
            .context("spawn ocr worker stderr reader failed")?;

        let mut worker = OcrWorker {
            _child: child,
            stdin,
            lines: rx,
            poisoned: false,
        };
        wait_ready(&mut worker)?;
        Ok(OcrEngine {
            worker: Mutex::new(worker),
            next_id: AtomicU64::new(1),
        })
    }

    fn ocr_png(&self, png: &[u8], deadline: Instant) -> Result<String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let payload = serde_json::json!({
            "id": id,
            "op": "ocr",
            "png_base64": base64::engine::general_purpose::STANDARD.encode(png),
        });
        let mut line = serde_json::to_vec(&payload)?;
        line.push(b'\n');

        let mut worker = self.worker.lock().expect("ocr worker lock poisoned");
        if worker.poisoned {
            bail!("ocr worker is poisoned after a previous failure");
        }
        worker
            .stdin
            .write_all(&line)
            .and_then(|_| worker.stdin.flush())
            .context("ocr worker stdin write failed")?;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                worker.poisoned = true;
                bail!("ocr worker response past the observation deadline");
            }
            match worker.lines.recv_timeout(remaining) {
                Ok(line) if line.id == Some(id) => {
                    if line.ok == Some(true) {
                        return Ok(line.text.unwrap_or_default());
                    }
                    bail!(
                        "{}",
                        line.error
                            .unwrap_or_else(|| "worker returned failure".to_string())
                    );
                }
                // 迟到/错位响应（上一次超时的残迹）：跳过，不影响本次。
                Ok(_) => continue,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    worker.poisoned = true;
                    bail!("ocr worker response timeout");
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    worker.poisoned = true;
                    bail!("ocr worker exited unexpectedly");
                }
            }
        }
    }
}

/// 读启动事件直到 ready；failed/超时/提前退出都算启动失败。
fn wait_ready(worker: &mut OcrWorker) -> Result<()> {
    let started = Instant::now();
    loop {
        let remaining = BOOT_TIMEOUT.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            bail!("ocr worker boot timed out");
        }
        match worker.lines.recv_timeout(remaining) {
            Ok(line) => match line.event.as_deref() {
                Some("ready") => return Ok(()),
                Some("failed") => {
                    bail!(
                        "ocr worker reported failed: {}",
                        line.error.unwrap_or_default()
                    )
                }
                // boot 或其它行：继续等。
                _ => continue,
            },
            Err(mpsc::RecvTimeoutError::Timeout) => bail!("ocr worker boot timed out"),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                bail!("ocr worker exited during boot")
            }
        }
    }
}

static ENGINE: OnceLock<Result<OcrEngine, String>> = OnceLock::new();

fn engine() -> Result<&'static OcrEngine, &'static str> {
    ENGINE
        .get_or_init(|| {
            let result = OcrEngine::spawn().map_err(|error| format!("{error:#}"));
            if let Err(error) = &result {
                log::warn!("[screen-ocr] ocr worker failed to start: {error}");
            }
            result
        })
        .as_ref()
        .map_err(|error| error.as_str())
}

/// 同步入口：PNG 字节 → OCR 文本（可能为空字符串）。失败返回 None 并记状态日志。
pub(crate) fn ocr_png(png: &[u8], deadline: Instant) -> Option<String> {
    let engine = engine().ok()?;
    match engine.ocr_png(png, deadline) {
        Ok(text) => Some(text),
        Err(error) => {
            log::info!("[screen-ocr] worker ocr failed: {error}");
            None
        }
    }
}
