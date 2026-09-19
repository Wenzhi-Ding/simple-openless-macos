//! RapidOCR worker 常驻进程托管（全同步：屏幕 OCR 的观测线程不走 tokio）。
//!
//! worker 首次使用时惰性拉起（Python 冷启动 + 引擎初始化约一两秒；启动有
//! 独立的 BOOT_TIMEOUT 预算，不占本次观测的 5 秒截止——冷启动偏慢时本次
//! 观测按超时放弃，但启动完成的 worker 保温留给下一次），之后常驻。协议
//! 与 qwen3_worker.py 相同：stdin/stdout 上换行分隔 JSON；stdout 由专用
//! reader 线程推进 mpsc；请求路径是「加锁 → 写一行 → 按 id 等响应行」，
//! 因此在途请求至多一个，响应不会交错。
//!
//! 失败后自愈：写失败、响应超时、worker 退出之后，当前进程被杀死丢弃，
//! 下一次请求自动重新拉起 worker（每段听写至多一次重建，不会连环拉起坏
//! 进程）。2026-09-19 之前是「一次失败永久毒化、直到重启应用才恢复」：
//! 一次系统繁忙造成的 5 秒超时把当天剩余全部 OCR 关停，用户裁定改为自动
//! 重建。普通日志只记状态不记图片内容。

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use base64::Engine as _;
use serde::Deserialize;

/// worker 启动 + 引擎初始化的上限（Python 冷启动 + RapidOCR 加载本地 ONNX）。
/// 独立预算，不受调用方观测截止约束：启动慢只放弃本次观测，不杀死半成品
/// worker，否则机器持续繁忙时会卡在「启动 → 超时 → 杀掉 → 再启动」循环里。
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
    child: Child,
    stdin: ChildStdin,
    lines: mpsc::Receiver<WorkerLine>,
}

/// 丢弃即杀进程并回收：失败重建或进程退出后，旧 worker 不得残留成孤儿。
impl Drop for OcrWorker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// 当前常驻 worker；None 表示尚未拉起，或上一次失败后已丢弃（下次请求重建）。
static WORKER: Mutex<Option<OcrWorker>> = Mutex::new(None);
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn spawn_worker() -> Result<OcrWorker> {
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
        child,
        stdin,
        lines: rx,
    };
    wait_ready(&mut worker)?;
    Ok(worker)
}

/// 读启动事件直到 ready；failed/超时/提前退出都算启动失败（本次 worker 随之
/// 丢弃，下次请求从头再拉）。
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

/// 一次请求-响应往返；任何失败都由调用方丢弃当前 worker（下次请求重建）。
fn roundtrip(worker: &mut OcrWorker, png: &[u8], deadline: Instant) -> Result<String> {
    let id = NEXT_ID.fetch_add(1, Ordering::SeqCst);
    let payload = serde_json::json!({
        "id": id,
        "op": "ocr",
        "png_base64": base64::engine::general_purpose::STANDARD.encode(png),
    });
    let mut line = serde_json::to_vec(&payload)?;
    line.push(b'\n');

    worker
        .stdin
        .write_all(&line)
        .and_then(|_| worker.stdin.flush())
        .context("ocr worker stdin write failed")?;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
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
                bail!("ocr worker response timeout");
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                bail!("ocr worker exited unexpectedly");
            }
        }
    }
}

/// 同步入口：PNG 字节 → OCR 文本（可能为空字符串）。失败返回 None 并记状态日志；
/// 失败实例当场丢弃，下一次调用自动重建 worker——单次抖动不再关停后续 OCR。
pub(crate) fn ocr_png(png: &[u8], deadline: Instant) -> Option<String> {
    let mut guard = WORKER.lock().expect("ocr worker lock poisoned");
    if guard.is_none() {
        match spawn_worker() {
            Ok(worker) => *guard = Some(worker),
            Err(error) => {
                log::info!("[screen-ocr] ocr worker start failed: {error:#}");
                return None;
            }
        }
    }
    // 冷启动可能已越过本次观测截止：worker 保温留给下一次，本次放弃。带已过的
    // 截止进 roundtrip 会把刚启动好的健康 worker 误杀，形成启动循环。
    if Instant::now() >= deadline {
        return None;
    }
    let worker = guard.as_mut().expect("worker presence ensured above");
    match roundtrip(worker, png, deadline) {
        Ok(text) => Some(text),
        Err(error) => {
            log::info!(
                "[screen-ocr] worker ocr failed: {error:#} (worker dropped; will respawn on next use)"
            );
            *guard = None;
            None
        }
    }
}
