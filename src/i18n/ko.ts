export const ko = {
  "common": {
    "loading": "로딩 중…",
    "retry": "다시 시도",
    "refresh": "새로고침",
    "clear": "지우기",
    "copy": "복사",
    "delete": "삭제",
    "cancel": "취소",
    "close": "닫기",
    "copied": "복사됨",
    "operationFailed": "작업 실패",
    "add": "추가",
    "durationSeconds": "{{value}}초",
    "durationMillis": "{{value}}ms",
    "durationMinutes": "{{value}}분"
  },
  "capsule": {
    "thinking": "thinking",
    "cancelled": "취소됨",
    "error": "오류 발생",
    "inserted": "{{count}}자 입력됨"
  },
  "nav": {
    "overview": "개요",
    "history": "기록",
    "vocab": "어휘",
    "more": "더보기"
  },
  "shell": {
    "betaTag": "BETA",
    "navHint": {
      "overview": "상태 개요: 사용량 통계, 제공자 및 권한 상태",
      "history": "받아쓰기 기록: 과거 전사 검색·재생·복사",
      "vocab": "어휘: 고유명사 인식률을 높이는 사용자 지정 핫워드",
      "settings": "환경설정: 단축키, 제공자, 개인정보 및 업데이트"
    },
    "footer": {
      "settings": "설정",
      "version": "버전 {{version}}",
      "helpPopover": {
      }
    },
    "screenOcrBanner": {
    }
  },
  "onboarding": {
    "screenPermissionDesc": "선택 사항으로 로컬 화면 OCR에 사용됩니다. 허용하지 않아도 받아쓰기는 가능합니다.",
    "skipScreenPermission": "화면 권한 없이 받아쓰기 계속",
    "accessibilityDesc": "전역 단축키(현재 단축키: {{trigger}}) 감지와 인식 결과를 커서 위치에 입력하기 위해 사용합니다.",
    "micTitle": "마이크",
    "micDesc": "음성 입력을 캡처하기 위해 사용합니다.",
    "micNoDeviceHint": "마이크가 감지되지 않습니다. 마이크를 연결하고 활성화한 후 다시 시도하세요.",
    "footerHint": "모두 허용하면 안내가 닫힙니다. 선택 사항인 화면 권한은 아래에서 건너뛸 수 있습니다.",
    "continueToSettings": "설정 열기"
  },
  "permissions": {
    "needsAttention": "확인 또는 처리가 필요합니다: {{names}}. 이미 허용된 항목은 다시 허용할 필요가 없습니다."
  },
  "overview": {
    "title": "오늘 개요",
    "asrKind": "ASR 음성",
    "llmKind": "LLM 모델",
    "modelError": "오류",
    "modelState": {
      "notFound": "찾을 수 없음",
      "notLoaded": "로드되지 않음",
      "loading": "로드 중…",
      "loaded": "로드됨",
      "failed": "실패"
    },
    "metricChars": "오늘 글자 수",
    "metricSegments": "{{count}} 세그먼트",
    "metricDuration": "오늘 총 시간",
    "metricAvg": "평균 세그먼트",
    "metricAvgTrend": "오늘 평균",
    "metricNoData": "데이터 없음",
    "historyLoadError": "기록 로드 실패",
    "metricTotal": "누적 기록",
    "metricTotalTrend": "로컬 보관(상한 200)",
    "activityTitle": "연간 활동",
    "activityCount": "{{count}}회 받아쓰기",
    "activityLoadError": "활동 데이터 로드 실패",
    "period": {
      "ariaLabel": "집계 기간",
      "last7Days": "최근 7일",
      "last30Days": "최근 30일",
      "dailyAverage": "일평균 {{value}}",
      "minutes": "{{value}}분",
      "hoursMinutes": "{{hours}}시간 {{minutes}}분"
    },
    "metricName": {
      "ariaLabel": "지표",
      "count": "건수",
      "chars": "글자 수",
      "duration": "시간"
    },
    "recentTitle": "최근 인식",
    "recentAll": "전체 보기 →",
    "recentEmpty": "아직 기록이 없습니다. {{trigger}} 를 눌러 첫 녹음을 시작하세요.",
    "recentLoadFailed": "최근 인식 기록을 불러올 수 없습니다. 다시 시도해 주세요.",
    "historyRetry": "다시 시도",
    "weekDays": [
      "일",
      "월",
      "화",
      "수",
      "목",
      "금",
      "토"
    ],
    "inAppDictation": {
    }
  },
  "history": {
    "exportError": "녹음을 내보내지 못했습니다. 다시 시도해 주세요.",
    "kicker": "HISTORY",
    "title": "기록",
    "desc": "로컬에 저장된 인식 기록.",
    "summary": "총 {{total}}건 · 표시 {{shown}}",
    "searchPlaceholder": "기록 검색…（{{shortcut}}）",
    "searchNoMatch": "“{{query}}”과(와) 일치하는 항목이 없습니다.",
    "empty": "기록이 없습니다. {{trigger}} 를 눌러 한 번 녹음해 보세요.",
    "loadFailed": "기록 로드 실패: {{err}}",
    "retry": "다시 시도",
    "clearFailed": "기록 비우기 실패: {{err}}",
    "deleteFailed": "항목 삭제 실패: {{err}}",
    "copyFailed": "복사 실패: {{err}}",
    "playRecording": "녹음 재생",
    "audioLoading": "로딩 중…",
    "audioDecodeFailed": "오디오 디코딩 실패: {{err}}",
    "exportRecording": "녹음 내보내기",
    "exportFailed": "내보내기 실패: {{err}}",
    "retranscribe": "다시 인식",
    "retranscribing": "인식 중…",
    "retranscribeFailed": "다시 인식 실패: {{err}}",
    "rawLabel": "원본 전사",
    "resultLabel": "정리 결과",
    "rawEmpty": "(비어 있음)",
    "selectHint": "왼쪽에서 하나를 선택하여 자세히 보기.",
    "recorded": "녹음 {{duration}}",
    "stepAsr": "인식",
    "stepAsrHint": "키를 뗀 후 인식 결과를 기다린 시간. 스트리밍 인식은 녹음 중에 변환하므로 보통 녹음 시간보다 훨씬 짧습니다.",
    "stepPolish": "다듬기",
    "stepInsert": "삽입",
    "chars": "{{count}}자",
    "vocabHits": "핫워드 {{count}}개",
    "inserted": "입력됨",
    "pasteSent": "붙여넣기 시도됨",
    "copiedFallback": "복사됨({{shortcut}} 필요)",
    "insertFailed": "입력 실패",
    "diagnostics": {
      "title": "润色 진단",
      "notSaved": "진단 스냅샷 없음(구버전 기록, 캡처 없는 경로, 재전사 후 삭제)",
      "stageLabel": "단계",
      "model": "모델",
      "templateLabel": "렌더 경로",
      "addBos": "BOS 주입",
      "tokens": "토큰",
      "tokenLine": "prompt {{prompt}} / 컨텍스트 {{ctx}} / 출력 예산 {{budget}}",
      "contextCursor": "커서 컨텍스트",
      "contextOcr": "화면 OCR",
      "contextTextChars": "{{count}}자",
      "notCalledReason": "미호출 사유",
      "error": "실패 요약",
      "promptSection": "최종 prompt(모델에 실제로 보낸 내용)",
      "messagesSection": "구조화 메시지({{count}}개, 실제 이전 턴 포함)",
      "copyPrompt": "prompt 복사",
      "copyAll": "전체 진단 복사",
      "outcome": {
        "succeeded": "성공",
        "failed": "실패",
        "notCalled": "모델 미호출"
      },
      "stage": {
        "notCalled": "미호출",
        "modelMissing": "모델 없음",
        "modelLoadFailed": "모델 로드 실패",
        "renderFailed": "prompt 렌더 실패",
        "rendered": "렌더 완료",
        "tokenized": "토큰화 완료",
        "prefilled": "prefill 완료",
        "generating": "생성 중",
        "succeeded": "생성 완료"
      },
      "template": {
        "gemma4Canonical": "Gemma 4 canonical(Rust 렌더, enable_thinking=false)",
        "ggufChatTemplate": "GGUF chat template(apply_chat_template)"
      },
      "addBosValue": {
        "injected": "예(tokenizer AddBos::Always, 본문에 <bos> 없음)",
        "off": "아니요"
      },
      "status": {
        "included": "채택됨",
        "notAdopted": "캡처만 됨/미채택",
        "disabled": "스위치 꺼짐",
        "notSupported": "미지원 플랫폼",
        "notStarted": "시작 실패",
        "permissionDenied": "권한 없음",
        "sensitiveBlocked": "민감 차단",
        "targetChanged": "대상 변경",
        "timeout": "시간 초과",
        "notReady": "준비 안 됨",
        "empty": "결과 없음",
        "failed": "실패",
        "blocked": "안전 게이트 차단",
        "unavailable": "사용 불가",
        "unknown": "알 수 없음"
      }
    },
    "confirmClear": "전체 {{count}}건의 기록을 삭제하시겠습니까? 이 작업은 되돌릴 수 없습니다.",
    "backToList": "목록으로",
    "repolish": {
      "title": "다시 정리",
      "hint": "원본 전사를 다시 정리합니다. 결과는 여기에서만 표시되며 기록을 덮어쓰지 않습니다.",
      "retrying": "정리 중…",
      "failed": "다시 정리 실패: {{err}}",
      "timeout": "로컬 모델의 처리 시간이 초과되었습니다. 다시 시도해 주세요.",
      "empty": "(모델이 빈 결과를 반환했습니다)",
      "clear": "결과 지우기"
    }
  },
  "vocabCard": {
  },
  "insertFallbackCard": {
    "copy": "복사",
    "copied": "복사됨",
    "copyFailed": "복사 실패",
    "dismiss": "닫기"
  },
  "vocab": {
    "kicker": "VOCABULARY",
    "title": "어휘",
    "desc": "새 단어나 전문 용어를 추가하여 인식 정확도 향상.",
    "sectionTitle": "항목",
    "placeholder": "단어를 입력하고 Enter 또는 추가 클릭…",
    "tip": "한영 혼용 지원 · 숫자로 시작하면 그대로 인식 · 적중 횟수 자동 카운트",
    "loadFailed": "로드 실패: {{err}}",
    "empty": "어휘가 없습니다. 위에 새 단어나 전문 용어를 입력하면 받아쓰기 시 우선 매칭됩니다.",
    "tipDisabled": "클릭하여 비활성화",
    "tipEnabled": "클릭하여 활성화",
    "removeAria": "삭제",
  },
  "settings": {
    "dataStorage": {
      "title": "데이터 저장",
      "screenOcrPermissionLabel": "화면 기록 권한",
    },
    "debug": {
      "title": "디버그 도구",
      "cursorProbeLabel": "커서 문맥 프로브",
      "cursorProbeDesc": "누른 뒤 카운트다운 안에 대상 앱으로 전환해 입력란을 클릭하세요. 그곳의 커서 주변 원문을 읽어, 어떤 앱이 읽히고 어떤 앱이 안전 게이트에 막히는지 확인할 수 있습니다. 한 번만 읽으며 어떤 제공자에게도 보내지 않습니다.",
      "cursorProbeBtn": "프로브 (5초 후)",
      "cursorProbeCountdown": "{{n}}초 후 읽기…"
    },
    "recording": {
      "title": "녹음 및 입력",
      "desc": "전역 녹음의 단축키와 트리거 방식을 정의합니다.",
      "hotkeyLabel": "녹음 단축키",
      "silenceAutoStopLabel": "침묵 시 자동 중지",
      "silenceAutoStopDesc": "음성이 감지된 후 선택한 시간 동안 침묵이 이어지면 녹음을 자동으로 종료하고 제출합니다. 말을 전혀 하지 않으면 10초 후 취소됩니다. 기본적으로 꺼져 있습니다.",
      "silenceAutoStopSecondsLabel": "침묵 시간",
      "silenceAutoStopSecondsValue": "{{value}}초",
      "microphoneLabel": "기본 선택 마이크",
      "microphoneDesc": "우선 사용할 입력 장치를 선택합니다. 장치를 일시적으로 사용할 수 없으면 시스템 기본 마이크를 사용하고, 다시 연결되면 자동으로 우선 장치로 돌아갑니다.",
      "microphoneDefault": "시스템 기본 마이크",
      "microphoneLoadError": "마이크 로드 실패: {{message}}",
      "capsuleLabel": "녹음 캡슐",
      "capsuleDesc": "녹음 / 전사 중 화면 하단에 반투명 캡슐을 표시합니다.",
      "capsuleStyleLabel": "캡슐 스타일",
      "capsuleStyleSiri": "시리 광선 스타일",
      "capsuleStyleClassic": "Openless 기본 스타일",
      "muteDuringRecordingLabel": "녹음 중 음소거",
      "muteDuringRecordingDesc": "녹음 중 시스템 출력을 일시적으로 음소거하여 스피커 에코를 방지합니다.",
      "audioCueLabel": "녹음 시작음",
      "audioCueDesc": "단축키로 녹음을 시작할 때 합성된 짧은 알림음을 재생합니다. 캡슐이 숨겨져 있어도 재생됩니다.",
      "audioCuePreview": "미리듣기",
      "comboRecordBtn": "단축키 녹화",
      "comboResetBtn": "초기화",
      "comboMenuToggle": "더보기",
      "comboDisableHint": "핵심 단축키는 비활성화할 수 없습니다 (녹음에는 단축키가 필수입니다)",
      "comboRecordHint": "단축키 조합을 눌러 주세요…",
      "comboConflict": "이 단축키 조합은 사용할 수 없습니다",
      "historyRetentionLabel": "기록 보관 기간(일)",
      "historyMaxEntriesLabel": "기록 개수 상한",
      "polishContextWindowLabel": "대화 컨텍스트 윈도(분)",
      "recordAudioForDebugLabel": "원본 녹음 보관(디버그)",
      "audioRecordingMaxEntriesLabel": "원본 녹음 보관 개수",
      "audioRecordingMaxEntriesDesc": "로컬 보관 wav 파일 상한. 빈칸 = 200.",
      "startupGroupTitle": "시작",
      "startMinimizedLabel": "시작 시 메인 창 숨기기",
      "startupAtBoot": "부팅 시 자동 시작",
      "startupAtBootError": "자동 시작 전환 실패: {{message}}"
    },
    "shortcuts": {
      "confirm": "캡슐 입력 확정",
      "disable": "비활성화"
    },
    "permissions": {
      "title": "권한",
      "micLabel": "마이크",
      "accLabel": "접근성",
      "hotkeyLabel": "전역 단축키",
      "checking": "확인 중…",
      "checkFailed": "현재 확인할 수 없습니다. 다시 시도하세요.",
      "granted": "허용됨",
      "notApplicable": "권한 불필요",
      "denied": "허용되지 않음",
      "indeterminate": "미결정",
      "micNoDevice": "마이크가 감지되지 않음",
      "openSystem": "시스템 설정 열기",
      "grant": "허용",
      "hotkeyInstalled": "설치됨",
      "hotkeyStarting": "설치 중…",
      "hotkeyFailed": "감지 실패",
    },
    "language": {
      "title": "인터페이스 언어",
      "label": "언어",
      "followSystem": "시스템 따라가기",
      "zh": "简体中文",
      "zhTW": "繁體中文",
      "en": "English",
      "ja": "日本語 (Beta)",
      "ko": "한국어 (Beta)"
    },
    "theme": {
      "title": "모양",
      "label": "테마",
      "activityHeatmapLabel": "개요 페이지에 연간 활동 표시",
      "system": "시스템 따르기",
      "light": "라이트",
      "dark": "다크"
    }
  },
  "modal": {
    "sections": {
      "general": "일반",
      "privacy": "개인정보 및 권한",
      "advanced": "고급",
    },
    "about": {
      "localFirst": "완전한 로컬 처리",
      "exportErrorLog": "오류 로그 내보내기",
      "exportErrorLogBtn": "내보내기",
      "exporting": "내보내는 중…",
      "exportSuccess": "저장됨",
      "exportFailed": "내보내기 실패",
      "privacyDesc": "음성 인식, 다듬기 및 기록은 모두 이 Mac에서만 처리되며 업로드되지 않습니다.",
    }
  }
} as const;
