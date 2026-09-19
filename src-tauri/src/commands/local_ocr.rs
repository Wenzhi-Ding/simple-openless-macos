/// 常驻屏幕 OCR worker（RapidOCR）状态：运行时在盘、启动中、worker 存活、
/// 实际内存占用与最近一次失败原因。数据来自 `ocr_engine` 的无锁旁路快照，
/// 与听写识别互不阻塞。
#[tauri::command]
pub fn local_ocr_engine_status() -> Result<crate::ocr_engine::OcrEngineStatus, String> {
    Ok(crate::ocr_engine::engine_status())
}
