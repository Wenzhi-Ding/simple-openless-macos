// Browser development fixtures for the currently supported local macOS product.

export interface MockVocabItem { word: string; count: number; }
export interface MockHistoryItem { time: string; dur: string; preview: string; }

export const OL_DATA: { vocab: MockVocabItem[]; history: MockHistoryItem[] } = {
  vocab: [
    { word: 'OpenLess', count: 8 }, { word: 'macOS', count: 5 },
    { word: 'Qwen3-ASR', count: 3 }, { word: 'Gemma', count: 2 },
  ],
  history: [
    { time: '13:30', dur: '24″', preview: '明天上午十点和产品团队开会。' },
    { time: '13:25', dur: '18″', preview: '请整理今天的会议记录。' },
  ],
};
