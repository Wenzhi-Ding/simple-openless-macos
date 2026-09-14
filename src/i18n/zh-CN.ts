export const zhCN = {
  "common": {
    "loading": "加载中…",
    "retry": "重试",
    "refresh": "刷新",
    "clear": "清空",
    "copy": "复制",
    "delete": "删除",
    "cancel": "取消",
    "close": "关闭",
    "copied": "已复制",
    "operationFailed": "操作失败",
    "add": "添加",
    "durationSeconds": "{{value}} 秒",
    "durationMillis": "{{value}} 毫秒",
    "durationMinutes": "{{value}} 分钟"
  },
  "capsule": {
    "thinking": "thinking",
    "cancelled": "已取消",
    "error": "出错了",
    "inserted": "已插入 {{count}}"
  },
  "nav": {
    "overview": "概览",
    "history": "历史",
    "vocab": "词汇表",
    "more": "更多"
  },
  "shell": {
    "betaTag": "BETA",
    "navHint": {
      "overview": "状态总览：用量统计、提供商与权限健康检查",
      "history": "听写历史：搜索、回放与复制过往转写",
      "vocab": "词汇表：自定义热词，提升专有名词识别率",
      "settings": "偏好设置：快捷键、提供商、隐私与更新"
    },
    "footer": {
      "settings": "设置",
      "version": "版本 {{version}}",
      "helpPopover": {
      }
    },
    "screenOcrBanner": {
    }
  },
  "onboarding": {
    "screenPermissionDesc": "可选，用于本地屏幕 OCR；不授权仍可普通听写。",
    "skipScreenPermission": "暂不授权屏幕，继续听写",
    "accessibilityDesc": "用于监听全局快捷键（当前快捷键：{{trigger}}）并把识别结果写入光标位置。",
    "micTitle": "麦克风",
    "micDesc": "用于捕获你的语音输入。",
    "micNoDeviceHint": "未检测到麦克风，请连接并启用麦克风后重试。",
    "footerHint": "授权全部完成后引导自动关闭，也可在下方跳过可选屏幕授权。",
    "continueToSettings": "进入设置"
  },
  "permissions": {
    "needsAttention": "待检查或处理：{{names}}；已授权项目无需重新授权。"
  },
  "overview": {
    "title": "今日概览",
    "asrKind": "ASR 语音",
    "llmKind": "LLM 模型",
    "modelError": "错误",
    "modelState": {
      "notFound": "未找到",
      "notLoaded": "未加载",
      "loading": "加载中…",
      "loaded": "已加载",
      "failed": "失败"
    },
    "metricChars": "今日字数",
    "metricSegments": "{{count}} 段",
    "metricDuration": "今日总时长",
    "metricAvg": "平均段落",
    "metricAvgTrend": "今日均值",
    "metricNoData": "暂无数据",
    "historyLoadError": "历史读取失败",
    "metricTotal": "累计记录",
    "metricTotalTrend": "本机存档 (上限 200)",
    "activityTitle": "年度活动",
    "activityCount": "{{count}} 次听写",
    "activityLoadError": "活动数据读取失败",
    "period": {
      "ariaLabel": "统计周期",
      "last7Days": "近 7 天",
      "last30Days": "近 30 天",
      "dailyAverage": "日均 {{value}}",
      "minutes": "{{value}} 分钟",
      "hoursMinutes": "{{hours}} 小时 {{minutes}} 分"
    },
    "metricName": {
      "ariaLabel": "统计指标",
      "count": "条数",
      "chars": "字数",
      "duration": "时长"
    },
    "recentTitle": "最近识别",
    "recentAll": "全部记录 →",
    "recentEmpty": "还没有记录。按 {{trigger}} 开始第一次录音。",
    "recentLoadFailed": "无法读取最近识别，请重试。",
    "historyRetry": "重试",
    "weekDays": [
      "日",
      "一",
      "二",
      "三",
      "四",
      "五",
      "六"
    ],
    "inAppDictation": {
    }
  },
  "history": {
    "exportError": "导出录音失败，请重试。",
    "kicker": "HISTORY",
    "title": "历史记录",
    "desc": "本机保存的识别记录。",
    "summary": "共 {{total}} 条 · 显示 {{shown}}",
    "searchPlaceholder": "搜索转写内容…（{{shortcut}}）",
    "searchNoMatch": "没有匹配「{{query}}」的记录。",
    "empty": "还没有历史记录。按 {{trigger}} 录一段试试。",
    "loadFailed": "加载历史失败：{{err}}",
    "retry": "重试",
    "clearFailed": "清空失败：{{err}}",
    "deleteFailed": "删除失败：{{err}}",
    "copyFailed": "复制失败：{{err}}",
    "playRecording": "播放录音",
    "audioLoading": "加载中…",
    "audioDecodeFailed": "音频解码失败：{{err}}",
    "exportRecording": "导出录音",
    "exportFailed": "导出失败：{{err}}",
    "retranscribe": "重新转录",
    "retranscribing": "转录中…",
    "retranscribeFailed": "重新转录失败：{{err}}",
    "rawLabel": "原始转写",
    "resultLabel": "整理结果",
    "rawEmpty": "（空）",
    "selectHint": "左侧选一条查看详情。",
    "recorded": "录音 {{duration}}",
    "stepAsr": "识别",
    "stepAsrHint": "松键后等待识别结果的耗时。流式识别边录边转，此值通常远小于录音时长。",
    "stepPolish": "润色",
    "stepInsert": "插入",
    "chars": "{{count}} 字",
    "vocabHits": "{{count}} 个热词",
    "inserted": "已插入",
    "pasteSent": "已尝试粘贴",
    "copiedFallback": "已复制(需 {{shortcut}})",
    "insertFailed": "插入失败",
    "diagnostics": {
      "title": "润色诊断",
      "notSaved": "未保存诊断（旧版本记录、该链路未提供快照、或重新转录后已清除）",
      "stageLabel": "阶段",
      "model": "模型",
      "templateLabel": "渲染路径",
      "addBos": "BOS 注入",
      "tokens": "Token",
      "tokenLine": "prompt {{prompt}} / 上下文 {{ctx}} / 输出预算 {{budget}}",
      "contextCursor": "光标上下文",
      "contextOcr": "屏幕 OCR",
      "contextTextChars": "{{count}} 字",
      "notCalledReason": "未调用原因",
      "error": "失败摘要",
      "promptSection": "最终 prompt（实际送入模型）",
      "messagesSection": "结构化消息（{{count}} 条，含实际前几轮）",
      "copyPrompt": "复制 prompt",
      "copyAll": "复制完整诊断",
      "outcome": {
        "succeeded": "成功",
        "failed": "失败",
        "notCalled": "未调用模型"
      },
      "stage": {
        "notCalled": "未调用",
        "modelMissing": "模型缺失",
        "modelLoadFailed": "模型载入失败",
        "renderFailed": "prompt 渲染失败",
        "rendered": "已渲染",
        "tokenized": "已分词",
        "prefilled": "已 prefill",
        "generating": "生成中",
        "succeeded": "生成完成"
      },
      "template": {
        "gemma4Canonical": "Gemma 4 canonical（Rust 自渲染，enable_thinking=false）",
        "ggufChatTemplate": "GGUF chat template（apply_chat_template）"
      },
      "addBosValue": {
        "injected": "是（tokenizer AddBos::Always 注入，正文不含 <bos>）",
        "off": "否"
      },
      "status": {
        "included": "已采用",
        "notAdopted": "捕获未采用",
        "disabled": "开关关闭",
        "notSupported": "平台不支持",
        "notStarted": "启动失败",
        "permissionDenied": "无权限",
        "sensitiveBlocked": "敏感拦截",
        "targetChanged": "目标漂移",
        "timeout": "超时",
        "notReady": "未就绪",
        "empty": "结果为空",
        "failed": "失败",
        "blocked": "安全闸门拦截",
        "unavailable": "不可用",
        "unknown": "未知"
      }
    },
    "confirmClear": "确定清空全部 {{count}} 条记录？此操作不可恢复。",
    "backToList": "返回列表",
    "repolish": {
      "title": "重新整理",
      "hint": "重新整理原始转写；结果仅供本次查看，不写回历史。",
      "retrying": "整理中…",
      "failed": "重新整理失败：{{err}}",
      "timeout": "本地模型未能及时完成整理，请稍后重试。",
      "empty": "（模型返回了空结果）",
      "clear": "清除结果"
    }
  },
  "vocabCard": {
  },
  "insertFallbackCard": {
    "copy": "复制",
    "copied": "已复制",
    "copyFailed": "复制失败",
    "dismiss": "关闭"
  },
  "vocab": {
    "kicker": "VOCABULARY",
    "title": "词汇表",
    "desc": "添加生词或专业术语，提高识别准确率。",
    "sectionTitle": "词条",
    "placeholder": "输入词语，按 Enter 或点添加…",
    "tip": "支持中英混合 · 数字开头按字面识别 · 命中次数自动计数",
    "loadFailed": "加载失败:{{err}}",
    "empty": "还没有词条。在上面输入一个生词或专业术语，让模型在听写时优先匹配。",
    "tipDisabled": "点击禁用此词条",
    "tipEnabled": "点击启用此词条",
    "removeAria": "删除",
  },
  "settings": {
    "dataStorage": {
      "title": "数据存储",
      "screenOcrPermissionLabel": "屏幕录制权限",
    },
    "debug": {
      "title": "调试工具",
      "cursorProbeLabel": "光标上下文探针",
      "cursorProbeDesc": "点一下，然后在倒计时内切到目标 app 并点进输入框——探针会读那里的光标附近原文，用来确认哪些 app 读得到、哪些被安全闸门拦住。只读一次，不发给任何服务商。",
      "cursorProbeBtn": "探测（5 秒后）",
      "cursorProbeCountdown": "{{n}} 秒后读取…"
    },
    "recording": {
      "title": "录音与输入",
      "desc": "全局录音的快捷键与触发方式。",
      "hotkeyLabel": "录音快捷键",
      "silenceAutoStopLabel": "静音后自动停止",
      "silenceAutoStopDesc": "检测到语音后，连续静音达到所选时长即自动结束并提交；一直没说话则 10 秒后取消。默认关闭。",
      "silenceAutoStopSecondsLabel": "静音时长",
      "silenceAutoStopSecondsValue": "{{value}} 秒",
      "microphoneLabel": "首选麦克风",
      "microphoneDesc": "选择优先输入设备。设备断开时自动切到系统默认。",
      "microphoneDefault": "系统默认麦克风",
      "microphoneLoadError": "麦克风列表读取失败：{{message}}",
      "capsuleLabel": "录音胶囊",
      "capsuleDesc": "录音 / 转写时显示屏幕底部胶囊。",
      "capsuleStyleLabel": "胶囊样式",
      "capsuleStyleSiri": "流光 Siri 风格",
      "capsuleStyleClassic": "Openless 默认风格",
      "muteDuringRecordingLabel": "录音时静音",
      "muteDuringRecordingDesc": "录音期间临时静音系统输出，避免扬声器回音。",
      "audioCueLabel": "录音提示音",
      "audioCueDesc": "按下热键开始录音时播放一段合成提示音，提醒已开始录音。胶囊隐藏时也会响。",
      "audioCuePreview": "试听",
      "comboRecordBtn": "录制快捷键",
      "comboResetBtn": "重置",
      "comboMenuToggle": "更多操作",
      "comboDisableHint": "核心快捷键不可停用，录音必须绑定一个热键",
      "comboRecordHint": "请按下快捷键组合…",
      "comboConflict": "该快捷键组合不可用",
      "historyRetentionLabel": "历史保留天数",
      "historyMaxEntriesLabel": "历史条数上限",
      "polishContextWindowLabel": "对话上下文窗口（分钟）",
      "recordAudioForDebugLabel": "保留原始录音（调试）",
      "audioRecordingMaxEntriesLabel": "原始录音保留条数",
      "audioRecordingMaxEntriesDesc": "本地保留 wav 文件数上限，留空 = 200。",
      "startupGroupTitle": "启动",
      "startMinimizedLabel": "启动时静默运行",
      "startupAtBoot": "开机自启",
      "startupAtBootError": "开机自启切换失败：{{message}}"
    },
    "shortcuts": {
      "confirm": "胶囊确认插入",
      "disable": "停用"
    },
    "permissions": {
      "title": "权限",
      "micLabel": "麦克风",
      "accLabel": "辅助功能",
      "hotkeyLabel": "全局快捷键",
      "checking": "检查中…",
      "checkFailed": "暂时无法确认，请重试",
      "granted": "已授权",
      "notApplicable": "无需授权",
      "denied": "未授权",
      "indeterminate": "未确定",
      "micNoDevice": "未检测到麦克风",
      "openSystem": "打开系统设置",
      "grant": "授权",
      "hotkeyInstalled": "已安装",
      "hotkeyStarting": "安装中…",
      "hotkeyFailed": "监听失败",
    },
    "language": {
      "title": "界面语言",
      "label": "语言",
      "followSystem": "跟随系统",
      "zh": "简体中文",
      "zhTW": "繁體中文",
      "en": "English",
      "ja": "日本語 (Beta)",
      "ko": "한국어 (Beta)"
    },
    "theme": {
      "title": "外观",
      "label": "主题",
      "activityHeatmapLabel": "概览页显示年度活动热力图",
      "system": "跟随系统",
      "light": "浅色",
      "dark": "深色"
    }
  },
  "modal": {
    "sections": {
      "general": "通用",
      "privacy": "隐私与权限",
      "advanced": "高级",
    },
    "about": {
      "localFirst": "完全本地处理",
      "exportErrorLog": "导出错误日志",
      "exportErrorLogBtn": "导出",
      "exporting": "导出中…",
      "exportSuccess": "已保存",
      "exportFailed": "导出失败",
      "privacyDesc": "识别、润色和历史记录均完全在此 Mac 本机处理，不会上传数据。",
    }
  }
} as const;
