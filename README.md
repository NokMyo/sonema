# Febius Sonema

**빠르고 정교한 오디오 작업 도구.**

Sonema는 한 곡을 녹음하고 편집하고 믹스해 납품 가능한 WAV로 만드는
과정에 집중한 Rust 네이티브 Audio DAW입니다. v0.1은 보컬과 실제 악기
녹음 중심의 작지만 완결된 상용 최소판입니다.

## v0.1 작업 흐름

- WAV, MP3, AAC, FLAC, OGG 등 오디오 불러오기
- 입력 장치 선택, 모노/스테레오 PCM 녹음과 입력 모니터링
- 여러 트랙, 파형, 클립 이동·트림·분할·복제·삭제, 박자 스냅
- 트랙 음량, 팬, 뮤트, 솔로, 녹음 대기, 실시간 레벨 미터
- 트랙별 하이패스, 3밴드 EQ, 컴프레서
- 재생, 정지, 탐색, 메트로놈, BPM/박자 격자
- 실행 취소/다시 실행과 30초 자동 복구
- 모든 미디어가 포함된 단일 `.sonema` 프로젝트 파일
- 16-bit, 24-bit PCM 또는 32-bit float WAV 출력

## 구조

```text
crates/
  sonema-core/    프로젝트 모델, 편집 규칙, 실행 취소
  sonema-dsp/     필터, EQ, 컴프레서, 미터
  sonema-audio/   CPAL 실시간 엔진, 디코딩, 녹음, 오프라인 출력
  sonema-format/  .sonema 컨테이너와 WAV 파일
  sonema-app/     egui 네이티브 데스크톱 화면
```

화면·오디오 장치·프로젝트 형식이 핵심 모델에 의존하고 서로 직접 얽히지
않습니다. 이후 MIDI, VST3/AU, 자동화, 테이크 컴핑을 별도 모듈로 추가할
수 있도록 프로젝트 형식에도 버전과 확장 영역을 두었습니다.

## 빌드

Rust stable이 필요합니다.

```bash
cargo run --locked -p sonema-app --release
cargo test --locked --workspace
```

Windows 배포 파일:

```powershell
pwsh scripts/package-windows.ps1
```

## 기본 단축키

| 동작 | 단축키 |
|---|---|
| 재생/일시정지 | `Space` |
| 녹음 시작/정지 | `R` |
| 저장 / 다른 이름으로 저장 | `Ctrl+S` / `Ctrl+Shift+S` |
| 열기 | `Ctrl+O` |
| 실행 취소 / 다시 실행 | `Ctrl+Z` / `Ctrl+Shift+Z` |
| 선택 클립 분할 | `S` |
| 선택 클립 복제 | `Ctrl+D` |
| 선택 클립 삭제 | `Delete` |
| 트랙 추가 | `Ctrl+T` |

## v0.1의 의도적인 범위

이번 버전은 오디오 트랙 DAW입니다. MIDI, 가상악기, VST3/AU 플러그인,
템포 자동화는 아직 지원하지 않으며 동작하지 않는 장식용 버튼도 넣지
않았습니다. 이 범위 안의 기능은 저장·재열기·최종 출력까지 이어집니다.
