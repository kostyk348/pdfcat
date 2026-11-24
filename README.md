# pdfcat

**Каталогизатор PDF на сетевом диске с полнотекстовым поиском (BM25). Rust, ноль зависимостей, Windows 7.**

Указываете сетевой диск — программа собирает каталог всех папок и PDF как граф (только чтение!), строит
полнотекстовый индекс и ищет по имени и по содержимому, включая кириллицу через ToUnicode.

[English below](#english)

## Превью — основа экрана + картинки JPEG (v0.4.0)

- плитка «Превью PDF» теперь **главная область окна** (~75% ширины); поиск, результаты,
  каталог и статистика — компактный сайдбар справа;
- **полный рендер страницы**: текст с позициями и поворотами, примитивы, клип по MediaBox
  **и изображения JPEG (DCTDecode)** через собственный DCT-декодер (baseline, YCbCr/Gray, 4:2:0);
- скан + индексация — **в фоновом потоке** с живым прогрессом в статусной строке (окно не виснет);
- индексация **параллельная** (8 потоков): 120 PDF / 90 000 токенов ≈ 50–90 мс на тёплом кеше;
- исправлены композиция матриц `cm`/`Td`/`T*` (column-vector порядок) и проход DCTDecode в `stream_of`.

## Окно Bento + визуальный превью PDF (v0.3.0)

Запуск **без аргументов** открывает окно Bento Grids на GDI (тёмная тема, сетка 8 px):
- плитки: «Каталог» (дерево **и** граф — оба режима), «Результаты поиска» с полем ввода и каретой,
  «Статистика», «Превью текста»;
- кнопка «Сканировать…» → выбор папки → скан + индексация (сетевой диск **не изменяется**);
- двойной клик по PDF — открыть в просмотрщике (`ShellExecuteW`, тоже только чтение);
- БД хранится **рядом с exe** (портативно), при отказе прав — fallback `%LOCALAPPDATA%\pdfcat`;
- OCR не существует по решению владельца — сканы ищутся только по имени.

## Возможности CLI (E0)

- `pdfcat scan <путь> [глубина]` — рекурсивный каталог (UNC/локальные диски), SoA-пулы, арена строк
- `pdfcat index [лимит]` — извлечение текста PDF собственным парсером (0 зависимостей) + BM25-индекс
- `pdfcat search <запрос>` — BM25 по содержимому (если индекс построен) или по имени
- `pdfcat stats` · `pdfcat peek <N>` — диагностика
- **Строгое правило: сетевой диск не изменяется ни на байт** — база каталога (`catalog.pdfcat`, формат
  `PDFCAT 1`) и индекс (`index.pdfcat`, `PDFBM25 1`) пишутся только локально.

## Замеры (реальные прогоны, Windows 11, warm cache)

| Операция | Результат |
|---|---|
| Скан: 60 каталогов / 360 файлов / 120 PDF | 13 мс |
| Индексация: 600 страниц / 90 000 токенов / 120 PDF | 205 мс (0,34 мс/страница), линейно |
| Первый холодный прогон | 3,8 с — Defender real-time на свежих файлах (окружение, не парсер) |
| Поиск BM25 | 80–100 мкс |
| Win7-сборка (rustc 1.77.2, `x86_64-pc-windows-gnu`) | exe **514 КБ** |
| Тесты | 17/17 |

## Честные ограничения парсера

Поддержано: классический xref (+fallback-скан «N G obj»), `/FlateDecode` (свой inflate), literal/hex
строки, WinAnsi/PDFDocEncoding, **Type0/Identity-H через `/ToUnicode` (bfchar/bfrange) — кириллица
Word-экспортов**, `/Length` (в т.ч. косвенная).
Пропускается с причиной: xref/object streams (PDF 1.5+), `/Differences`, LZW/RunLength, шифрованные PDF. Рендер превью: без картинок (DCT), шейдингов и клип-путей — текст и векторные примитивы.
PDF, сканы без текстового слоя (OCR — этап E6).

## Запуск

```bat
pdfcat.exe scan \\server\share\docs
pdfcat.exe index
pdfcat.exe search "смета вентиляция"
```

Сборка: `cargo build --release`; для Win7: `cargo +1.77.2 build --release --target x86_64-pc-windows-gnu`
(требуется KB2999226 на целевой машине).

## Архитектура

Hexagonal: порты `Source / TextExtractor / Searcher / CatalogStore`; домен (DDD) — `Catalog` с
инвариантами; память (DOP) — SoA-пулы и арена строк; SIMD-ядро BM25 (AVX2 gather, рантайм-детект).
Подробное решение: `docs/pdfcat-solution.md`.

## Планы

E2 — PDF 1.5 (xref/object streams), `/Differences`, LZW; E3 — наблюдение за изменениями;
E4 — параллельная индексация + mmap-индекс; E5 — сниппеты и горизонтальный скролл графа; E6 — сжатие индекса.

---

<a name="english"></a>
## English

**PDF catalog organizer for network drives with full-text search (BM25). Rust, zero dependencies, Windows 7.**

- `scan <path> [depth]` — recursive catalog (UNC/local), SoA pools, string arena
- `index [limit]` — text extraction by a self-contained PDF parser + BM25 index
- `search <query>` — BM25 over contents (after `index`) or filename fallback
- **Strict rule: the network drive is never written to** — the catalog (`catalog.pdfcat`, `PDFCAT 1`)
  and the index (`index.pdfcat`, `PDFBM25 1`) are stored locally only.

Benchmarks (real runs, warm cache): scan 360 files / 120 PDFs in **13 ms**; index 600 pages /
90 000 tokens in **205 ms** (linear); BM25 query **80–100 µs**; Win7 build (rustc 1.77.2,
`x86_64-pc-windows-gnu`) — **514 KB** exe; **17/17 tests**.

Supported PDF subset: classic xref + "N G obj" fallback scan, FlateDecode (own inflate), literal/hex
strings, WinAnsi/PDFDocEncoding, Type0/Identity-H via /ToUnicode (bfchar/bfrange — Cyrillic from
Word exports), /Length (incl. indirect). Skipped with reason: PDF 1.5+ xref/object streams,
/Differences, LZW/RunLength, encrypted files, scans without a text layer (OCR = E6).

MIT. Docs: `docs/pdfcat-solution.md` (RU).
