export const en = {
  "common": {
    "loading": "Loading…",
    "retry": "Retry",
    "refresh": "Refresh",
    "clear": "Clear",
    "copy": "Copy",
    "delete": "Delete",
    "cancel": "Cancel",
    "close": "Close",
    "copied": "Copied",
    "operationFailed": "Operation failed",
    "add": "Add",
    "durationSeconds": "{{value}}s",
    "durationMillis": "{{value}}ms",
    "durationMinutes": "{{value}}m"
  },
  "capsule": {
    "thinking": "thinking",
    "cancelled": "Cancelled",
    "error": "Something went wrong",
    "inserted": "Inserted {{count}}"
  },
  "nav": {
    "overview": "Overview",
    "history": "History",
    "vocab": "Vocabulary",
    "more": "More"
  },
  "shell": {
    "betaTag": "BETA",
    "navHint": {
      "overview": "Status overview: usage stats, provider & permission health",
      "history": "Dictation history: search, replay and copy past transcripts",
      "vocab": "Vocabulary: custom hotwords for better proper-noun accuracy",
      "settings": "Preferences: shortcuts, providers, privacy and updates"
    },
    "footer": {
      "settings": "Settings",
      "version": "Version {{version}}",
      "helpPopover": {
      }
    },
    "screenOcrBanner": {
    }
  },
  "onboarding": {
    "screenPermissionDesc": "Optional, for local screen OCR. Dictation works without it.",
    "skipScreenPermission": "Skip screen access and continue dictating",
    "accessibilityDesc": "Used to listen for the global hotkey (current shortcut: {{trigger}}) and write transcripts at the cursor.",
    "micTitle": "Microphone",
    "micDesc": "Used to capture your voice input.",
    "micNoDeviceHint": "No microphone detected. Connect and enable a microphone, then retry.",
    "footerHint": "This guide closes when all permissions are granted. You can skip optional screen access below.",
    "continueToSettings": "Open settings"
  },
  "permissions": {
    "needsAttention": "Check or resolve: {{names}}. Already granted permissions do not need to be granted again."
  },
  "overview": {
    "title": "Today's overview",
    "asrKind": "ASR",
    "llmKind": "LLM",
    "modelError": "Error",
    "modelState": {
      "notFound": "Not found",
      "notLoaded": "Not loaded",
      "loading": "Loading…",
      "loaded": "Loaded",
      "failed": "Failed"
    },
    "metricChars": "Characters today",
    "metricSegments": "{{count}} segments",
    "metricDuration": "Total duration today",
    "metricAvg": "Avg per segment",
    "metricAvgTrend": "Today's average",
    "metricNoData": "No data",
    "historyLoadError": "History load failed",
    "metricTotal": "Total records",
    "metricTotalTrend": "Local archive (max 200)",
    "activityTitle": "Annual activity",
    "activityCount": "{{count}} dictation(s)",
    "activityLoadError": "Activity data load failed",
    "period": {
      "ariaLabel": "Reporting period",
      "last7Days": "Last 7 days",
      "last30Days": "Last 30 days",
      "dailyAverage": "{{value}} / day",
      "minutes": "{{value}} min",
      "hoursMinutes": "{{hours}} h {{minutes}} min"
    },
    "metricName": {
      "ariaLabel": "Metric",
      "count": "Count",
      "chars": "Characters",
      "duration": "Duration"
    },
    "recentTitle": "Recent transcripts",
    "recentAll": "View all →",
    "recentEmpty": "No records yet. Press {{trigger}} to start your first recording.",
    "recentLoadFailed": "Could not load recent transcripts. Please retry.",
    "historyRetry": "Retry",
    "weekDays": [
      "Sun",
      "Mon",
      "Tue",
      "Wed",
      "Thu",
      "Fri",
      "Sat"
    ],
    "inAppDictation": {
    }
  },
  "history": {
    "exportError": "Failed to export the recording. Please try again.",
    "kicker": "HISTORY",
    "title": "History",
    "desc": "Locally stored transcripts.",
    "summary": "{{total}} total · showing {{shown}}",
    "searchPlaceholder": "Search transcripts… ({{shortcut}})",
    "searchNoMatch": "No entries match “{{query}}”.",
    "empty": "No history yet. Press {{trigger}} to record one.",
    "loadFailed": "Failed to load history: {{err}}",
    "retry": "Retry",
    "clearFailed": "Failed to clear history: {{err}}",
    "deleteFailed": "Failed to delete entry: {{err}}",
    "copyFailed": "Failed to copy: {{err}}",
    "playRecording": "Play recording",
    "audioLoading": "Loading…",
    "audioDecodeFailed": "Audio decode failed: {{err}}",
    "exportRecording": "Export recording",
    "exportFailed": "Failed to export: {{err}}",
    "retranscribe": "Retranscribe",
    "retranscribing": "Transcribing…",
    "retranscribeFailed": "Retranscribe failed: {{err}}",
    "rawLabel": "Original transcript",
    "resultLabel": "Edited result",
    "rawEmpty": "(empty)",
    "selectHint": "Select an entry on the left to see details.",
    "recorded": "Recorded {{duration}}",
    "stepAsr": "Transcribe",
    "stepAsrHint": "Time spent waiting for the transcript after key release. Streaming ASR transcribes while you speak, so this is usually much shorter than the recording.",
    "stepPolish": "Polish",
    "stepInsert": "Insert",
    "chars": "{{count}} chars",
    "vocabHits": "{{count}} vocab hits",
    "inserted": "Inserted",
    "pasteSent": "Paste sent",
    "copiedFallback": "Copied (use {{shortcut}})",
    "insertFailed": "Insert failed",
    "diagnostics": {
      "title": "Polish diagnostics",
      "notSaved": "No snapshot saved (older record, path without capture, or cleared after retranscription)",
      "stageLabel": "Stage",
      "model": "Model",
      "templateLabel": "Render path",
      "addBos": "BOS injection",
      "tokens": "Tokens",
      "tokenLine": "prompt {{prompt}} / context {{ctx}} / output budget {{budget}}",
      "contextCursor": "Cursor context",
      "contextOcr": "Screen OCR",
      "contextTextChars": "{{count}} chars",
      "notCalledReason": "Not called because",
      "error": "Failure summary",
      "promptSection": "Final prompt (actually sent to the model)",
      "messagesSection": "Structured messages ({{count}}, with actual prior turns)",
      "copyPrompt": "Copy prompt",
      "copyAll": "Copy full diagnostics",
      "outcome": {
        "succeeded": "Succeeded",
        "failed": "Failed",
        "notCalled": "Model not called"
      },
      "stage": {
        "notCalled": "Not called",
        "modelMissing": "Model missing",
        "modelLoadFailed": "Model load failed",
        "renderFailed": "Prompt render failed",
        "rendered": "Rendered",
        "tokenized": "Tokenized",
        "prefilled": "Prefilled",
        "generating": "Generating",
        "succeeded": "Generation complete"
      },
      "template": {
        "gemma4Canonical": "Gemma 4 canonical (Rust-side render, enable_thinking=false)",
        "ggufChatTemplate": "GGUF chat template (apply_chat_template)"
      },
      "addBosValue": {
        "injected": "Yes (tokenizer AddBos::Always; body has no <bos>)",
        "off": "No"
      },
      "status": {
        "included": "Included",
        "notAdopted": "Captured, not adopted",
        "disabled": "Switch off",
        "notSupported": "Unsupported platform",
        "notStarted": "Failed to start",
        "permissionDenied": "Permission denied",
        "sensitiveBlocked": "Sensitive block",
        "targetChanged": "Target changed",
        "timeout": "Timeout",
        "notReady": "Not ready",
        "empty": "Empty result",
        "failed": "Failed",
        "blocked": "Blocked by safety gate",
        "unavailable": "Unavailable",
        "unknown": "Unknown"
      }
    },
    "confirmClear": "Delete all {{count}} history entries? This cannot be undone.",
    "backToList": "Back to list",
    "repolish": {
      "title": "Edit again",
      "hint": "Edit the original transcript again. Results are shown only here and do not overwrite history.",
      "retrying": "Editing…",
      "failed": "Editing failed: {{err}}",
      "timeout": "The local model did not finish in time. Please try again.",
      "empty": "(the model returned an empty result)",
      "clear": "Clear results"
    }
  },
  "vocabCard": {
  },
  "insertFallbackCard": {
    "copy": "Copy",
    "copied": "Copied",
    "copyFailed": "Copy failed",
    "dismiss": "Dismiss"
  },
  "vocab": {
    "kicker": "VOCABULARY",
    "title": "Vocabulary",
    "desc": "Add terms or jargon to improve recognition accuracy.",
    "sectionTitle": "Entries",
    "placeholder": "Type a word, press Enter or click Add…",
    "tip": "Mixed Chinese/English supported · numeric prefixes are matched literally · hits counted automatically",
    "loadFailed": "Load failed: {{err}}",
    "empty": "No entries yet. Add a new term or piece of jargon above so the model can prioritize it.",
    "tipDisabled": "Click to disable this entry",
    "tipEnabled": "Click to enable this entry",
    "removeAria": "Remove",
  },
  "settings": {
    "dataStorage": {
      "title": "Data storage",
      "screenOcrPermissionLabel": "Screen recording permission",
    },
    "debug": {
      "title": "Debug tools",
      "cursorProbeLabel": "Cursor context probe",
      "cursorProbeDesc": "Click, then switch to the target app and click into a text field before the countdown ends. The probe reads the text around your cursor there, so you can see which apps are readable and which the safety gate blocks. One read, sent to no provider.",
      "cursorProbeBtn": "Probe (in 5s)",
      "cursorProbeCountdown": "Reading in {{n}}s…"
    },
    "recording": {
      "title": "Recording & input",
      "desc": "Global recording hotkey and trigger mode.",
      "hotkeyLabel": "Recording hotkey",
      "silenceAutoStopLabel": "Auto-stop after silence",
      "silenceAutoStopDesc": "After speech is detected, recording stops and submits automatically once silence lasts the chosen duration. Off by default.",
      "silenceAutoStopSecondsLabel": "Silence duration",
      "silenceAutoStopSecondsValue": "{{value}}s",
      "microphoneLabel": "Preferred microphone",
      "microphoneDesc": "Choose the preferred input device; falls back to system default when unavailable.",
      "microphoneDefault": "System default microphone",
      "microphoneLoadError": "Failed to load microphones: {{message}}",
      "capsuleLabel": "Recording capsule",
      "capsuleDesc": "Show a translucent capsule at the bottom of the screen while recording.",
      "capsuleStyleLabel": "Capsule style",
      "capsuleStyleSiri": "Shimmer Siri style",
      "capsuleStyleClassic": "OpenLess default style",
      "muteDuringRecordingLabel": "Mute while recording",
      "muteDuringRecordingDesc": "Temporarily mute system output during voice input to avoid speaker echo.",
      "audioCueLabel": "Recording start sound",
      "audioCueDesc": "Play a short synthesized chime when you press the hotkey to start recording. Plays even when the capsule is hidden.",
      "audioCuePreview": "Preview",
      "comboRecordBtn": "Record shortcut",
      "comboResetBtn": "Reset",
      "comboMenuToggle": "More options",
      "comboDisableHint": "Core hotkey cannot be disabled — recording needs a hotkey",
      "comboRecordHint": "Press your shortcut combination…",
      "comboConflict": "This shortcut combination is not available",
      "historyRetentionLabel": "History retention (days)",
      "historyMaxEntriesLabel": "Max history entries",
      "polishContextWindowLabel": "Polish context window (minutes)",
      "recordAudioForDebugLabel": "Keep raw recording (debug)",
      "audioRecordingMaxEntriesLabel": "Max raw recordings",
      "audioRecordingMaxEntriesDesc": "Max wav files retained locally. Blank = 200.",
      "startupGroupTitle": "Startup",
      "startMinimizedLabel": "Start minimized (no main window)",
      "startupAtBoot": "Launch at login",
      "startupAtBootError": "Failed to toggle launch at login: {{message}}"
    },
    "shortcuts": {
      "confirm": "Confirm capsule insertion",
      "disable": "Disable"
    },
    "permissions": {
      "title": "Permissions",
      "micLabel": "Microphone",
      "accLabel": "Accessibility",
      "hotkeyLabel": "Global hotkey",
      "checking": "Checking…",
      "checkFailed": "Unable to confirm right now. Please retry.",
      "granted": "Granted",
      "notApplicable": "Not required",
      "denied": "Not granted",
      "indeterminate": "Undetermined",
      "micNoDevice": "No microphone detected",
      "openSystem": "Open System Settings",
      "grant": "Grant",
      "hotkeyInstalled": "Installed",
      "hotkeyStarting": "Installing…",
      "hotkeyFailed": "Listener failed",
    },
    "language": {
      "title": "Interface language",
      "label": "Language",
      "followSystem": "Follow system",
      "zh": "简体中文",
      "zhTW": "繁體中文",
      "en": "English",
      "ja": "日本語 (Beta)",
      "ko": "한국어 (Beta)"
    },
    "theme": {
      "title": "Appearance",
      "label": "Theme",
      "activityHeatmapLabel": "Show annual activity heatmap on Overview",
      "system": "Follow system",
      "light": "Light",
      "dark": "Dark"
    }
  },
  "modal": {
    "sections": {
      "general": "General",
      "privacy": "Privacy & Permissions",
      "advanced": "Advanced",
    },
    "about": {
      "localFirst": "Fully local",
      "exportErrorLog": "Export error log",
      "exportErrorLogBtn": "Export",
      "exporting": "Exporting…",
      "exportSuccess": "Saved",
      "exportFailed": "Export failed",
      "privacyDesc": "Transcription, polishing, and history are processed entirely on this Mac and never uploaded.",
    }
  }
} as const;
