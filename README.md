<div align="center">
  <img src="macos/assets/weft-logo.svg" width="112" alt="Weft woven W logo" />
  <h1>Weft</h1>
  <p><strong>파일은 기억나는데, 어디 저장했는지 생각나지 않을 때.</strong></p>
  <p>Spotlight처럼 빠른 Mac 검색과, 에이전트가 근거를 찾을 수 있는 로컬 파일 탐색.</p>
  <p>
    <a href="README.en.md">English</a> ·
    <a href="https://github.com/CREE1116/weft/releases/latest">최신 릴리스</a> ·
    <a href="docs/TECHNICAL_REFERENCE.md">기술 문서</a>
  </p>
  <p>
    <a href="https://github.com/CREE1116/weft/releases/latest/download/Weft-1.0.0-arm64.dmg">Apple Silicon 다운로드</a> ·
    <a href="https://github.com/CREE1116/weft/releases/latest/download/Weft-1.0.0-x86_64.dmg">Intel 다운로드</a>
  </p>
  <p>
    <img alt="Latest release" src="https://img.shields.io/github/v/release/CREE1116/weft?display_name=tag&color=7655c9" />
    <img alt="Release downloads" src="https://img.shields.io/github/downloads/CREE1116/weft/total?color=7655c9" />
    <img alt="macOS 14+" src="https://img.shields.io/badge/macOS-14%2B-7655c9" />
    <img alt="Runs locally" src="https://img.shields.io/badge/search-local-7655c9" />
  </p>
</div>

---

Weft는 **내 파일을 위한 가벼운 검색 앱이자 에이전트용 로컬 검색 엔진**입니다. 파일명만 외우지 않아도 의미로 찾고, 검색 결과에서 원문 위치와 검증된 문맥을 확인할 수 있습니다. 검색과 임베딩은 기기 안에서 실행됩니다.

## Weft로 할 수 있는 일

- **Mac에서 바로 찾기** — `⌥ Space`를 눌러 검색하고, 파일명 또는 의미 검색을 전환합니다. 방향키로 결과를 살펴보고 Quick Look이나 Finder에서 확인합니다.
- **파일이 바뀌면 따라가기** — 폴더를 선택해 첫 인덱싱과 변경 감시를 시작하거나, 감시를 끄고 필요할 때 직접 갱신합니다. 인덱싱은 창을 닫아도 백그라운드에서 계속됩니다.
- **문서 내용까지 검색** — PDF, Word, PowerPoint, Excel, OpenDocument, EPUB, HWPX와 일반 텍스트를 인덱싱합니다.
- **필요한 동작을 키보드로 실행** — `@설정`, `@메모`, `@녹음`, `@web`과 파일·폴더 검색 범위를 사용합니다. 자주 쓰는 앱이나 URL도 별칭으로 연결할 수 있습니다.
- **에이전트가 근거를 찾기** — Codex와 Claude Code 등에 Weft 스킬을 설치해 폴더 탐색, 파일 선택, 원문 문맥 확인을 맡길 수 있습니다.
- **음성 메모와 Markdown 메모** — 다국어 음성을 기기에서 전사하고, Markdown 메모를 로컬에 저장해 검색합니다.

## 다운로드 및 시작

### macOS 앱

1. 위에서 Mac에 맞는 DMG를 내려받아 `Weft.app`을 Applications 폴더로 옮깁니다.
2. 앱을 열고 온보딩에서 검색할 폴더와 전역 단축키를 설정합니다.
3. 메뉴 막대의 Weft 아이콘에서 인덱싱을 시작하거나 변경 감시를 켭니다.
4. `⌥ Space`를 눌러 검색합니다.

macOS 14 이상을 지원합니다. Apple Silicon과 Intel용 앱을 각각 제공합니다. 공개 빌드는 Developer ID 공증 정보 없이 만들어질 수 있으며, Gatekeeper가 막으면 **시스템 설정 → 개인정보 보호 및 보안 → 그래도 열기**를 사용하세요.

### CLI 및 에이전트 스킬

저장소를 내려받은 뒤, 모델과 CLI 및 에이전트 스킬을 준비합니다. 아래 명령은 자동 인덱싱을 시작하지 않습니다.

```sh
git clone https://github.com/CREE1116/weft.git
cd weft
./scripts/install.sh --no-index
```

```sh
weft search "데이터베이스 재시도 정책" --json
weft index ~/Documents --watch
weft stats --json
```

검색용 텍스트 모델은 최초 설정 시 내려받습니다. 에이전트 스킬은 `.agents/skills/weft-semantic-search`에 포함돼 있으며, 설치 위치를 직접 선택할 수도 있습니다. 더 자세한 설치와 CLI 사용법은 [기술 참조](docs/TECHNICAL_REFERENCE.md)를 확인하세요.

## 개인정보와 인덱싱

- 기본 검색과 임베딩은 로컬에서 실행되며, 검색어와 파일 내용은 검색 서비스로 전송되지 않습니다.
- 인덱스에는 경로, 파일 해시, 위치 정보와 벡터를 저장합니다. 원본 문서 내용은 복사해 저장하지 않습니다.
- 앱은 온보딩만으로 전체 디스크를 스캔하지 않습니다. 사용자가 선택한 폴더에서 인덱싱을 시작하고, 설정에서 감시·수동 갱신·제외 형식과 인덱스 삭제를 관리합니다.
- 모델 최초 다운로드, GitHub 업데이트 확인, 사용자가 선택한 웹 검색은 네트워크를 사용할 수 있습니다. 미디어 임베딩과 음성 모델은 선택 기능이며 필요할 때 별도로 준비합니다.

## 지원 파일

| 종류 | 인덱싱 내용 |
| --- | --- |
| 소스 코드, Markdown, 텍스트, 설정 파일, 노트북 등 | 파일명과 문서 내용, 문맥 청크 |
| PDF, DOCX/DOCM, PPTX/PPTM, XLSX/XLSM | 추출 가능한 텍스트와 위치 정보 |
| ODT/ODP/ODS, EPUB, HWPX | 추출 가능한 텍스트 |
| 그 외 파일 및 지원 한도를 넘는 파일 | 파일명·경로 기반 검색 |
| 이미지·오디오·비디오 | 선택형 미디어 모델을 준비한 경우 내용 임베딩 |

문서 파싱 한도와 형식별 제약, 이미지·오디오 전사 조건은 [기술 참조](docs/TECHNICAL_REFERENCE.md)에 정리했습니다. 파일명 검색은 파일 내용을 색인하지 않은 결과일 수 있으며, Weft는 이를 구분해 표시합니다.

## 속도

Apple M4 Pro에서 진행한 7회 비교 측정의 중앙값 기준으로, 최적화 빌드는 이전 로컬 빌드보다 모델 로딩이 **45.6%**, mmap 벡터 검색이 **55.0%** 빨랐습니다. 짧은 검색의 중앙값은 약 **11.6 ms**였습니다. 이는 특정 Mac과 32,636개 파일 인덱스에서 측정한 개발 벤치마크이며, 모든 기기나 데이터에서 보장되는 수치는 아닙니다. [측정 환경과 전체 결과](eval/M4PRO_PERFORMANCE_2026-10-12.md)

## 프로젝트

- [릴리스 및 변경 기록](https://github.com/CREE1116/weft/releases)
- [기술 참조와 CLI 설명](docs/TECHNICAL_REFERENCE.md)
- [성능 측정](eval/M4PRO_PERFORMANCE_2026-10-12.md)
- [문서 형식 검증](eval/DOCUMENT_RESULTS.md)
- [문제 제보](https://github.com/CREE1116/weft/issues)

Weft는 파일 검색 도구입니다. 의미 검색 결과는 관련 가능성을 순위화하며, 내용의 정확성이나 완전성을 보증하지 않습니다. 중요한 근거는 원본 파일에서 확인하세요.
