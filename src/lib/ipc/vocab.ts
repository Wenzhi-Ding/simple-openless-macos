import type { DictionaryEntry } from "../types"
import { invokeOrMock } from "./shared"
import { mockVocab } from "./mock-data"

export function listVocab(): Promise<DictionaryEntry[]> {
    return invokeOrMock("list_vocab", undefined, () => mockVocab)
}

export function addVocab(
    phrase: string,
    note?: string,
): Promise<DictionaryEntry> {
    return invokeOrMock("add_vocab", { phrase, note }, () => ({
        id: `vocab-new-${Date.now()}`,
        phrase,
        note: note ?? null,
        enabled: true,
        hits: 0,
        createdAt: new Date().toISOString(),
    }))
}

export function removeVocab(id: string): Promise<void> {
    return invokeOrMock("remove_vocab", { id }, () => undefined)
}

export function setVocabEnabled(id: string, enabled: boolean): Promise<void> {
    return invokeOrMock("set_vocab_enabled", { id, enabled }, () => undefined)
}

export function copyTextToClipboard(text: string): Promise<void> {
    return invokeOrMock("copy_text_to_clipboard", { text }, () => undefined)
}

/** 落字失败兜底卡片关掉了（用户点关闭 / TTL 到时）。 */
export function dismissInsertFallbackCard(): Promise<void> {
    return invokeOrMock("dismiss_insert_fallback_card", undefined, () => undefined)
}

/** 把浏览器真实折行后的卡片高度同步给原生共享窗口。 */
export function reportInsertFallbackCardHeight(
    presentationId: number,
    height: number,
): Promise<void> {
    return invokeOrMock(
        "report_insert_fallback_card_height",
        { presentationId, height },
        () => undefined,
    )
}
