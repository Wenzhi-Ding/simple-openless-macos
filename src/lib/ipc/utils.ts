import { isTauri, invokeOrMock } from "./shared"

export { isTauri }

/**
 * 让用户选 save 路径并把当前会话日志（openless.log）复制过去。
 * 浏览器开发模式下走 mock 不实际写盘。返回最终 save 的绝对路径，取消选择则返回 null。
 *
 * 文件名已带 .log，足够标识类型。
 */
export async function exportErrorLog(
    suggestedFileName: string,
): Promise<string | null> {
    if (!isTauri) {
        return `~/Downloads/${suggestedFileName}`
    }
    const { save } = await import("@tauri-apps/plugin-dialog")
    const target = await save({
        defaultPath: suggestedFileName,
        filters: [{ name: "Log", extensions: ["log", "txt"] }],
    })
    if (!target) return null
    await invokeOrMock<void>(
        "export_error_log",
        { targetPath: target },
        () => undefined,
    )
    return target
}

/** 探一次「宿主 app 光标周围的正文」。**调试用，不接产品链路。**
 *
 *  `delayMs` 是这个入口能用起来的关键：从设置页点按钮时前台 app 是 OpenLess 自己，
 *  读到的永远是我们自己的窗口。给几秒延迟，用户才有时间切到备忘录 / VS Code / 微信
 *  里点进输入框，探针在那时才真正开始读。 */
export async function debugReadCursorContext(
    delayMs: number,
): Promise<import("../types").HostDocumentReadResult> {
    const { invoke } = await import("@tauri-apps/api/core")
    return invoke("debug_read_cursor_context", { delayMs })
}
