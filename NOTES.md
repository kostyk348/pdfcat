# pdfcat v0.3.0 — визуальный превью PDF с пролистыванием

## Русский
Превью в окне теперь показывает **саму страницу PDF** (свой растеризатор: текст с позициями
и поворотами, линии, прямоугольники, цвета; клип по MediaBox). Навигация: ◀ ▶ по страницам,
колесо — прокрутка листа, масштаб по ширине плитки. Плюс всё из v0.2.0: окно Bento,
дерево и граф, BM25-поиск с кириллицей, БД рядом с exe, диск только читается.

Честные ограничения рендера: без картинок (DCT/CCITT), шейдингов, клип-путей и встроенных
шрифтов (замена системным Segoe UI). Тесты 18/18. Win7: `pdfcat-0.3.0-win7-x64.exe` (595 КБ).

## English
The preview tile now renders **the actual PDF page** (own content-stream rasterizer: positioned
text with 90° rotations, lines, rectangles, colors; MediaBox clipping). Navigation: ◀ ▶ pages,
wheel to scroll, fit-width by default. Plus everything from v0.2.0: Bento window, tree and graph
views, Cyrillic BM25 search, DB next to the exe, read-only drive.

Honest renderer limits: no images (DCT/CCITT), no shadings, no clip paths, no embedded fonts
(substituted with system Segoe UI). Tests 18/18. Win7: `pdfcat-0.3.0-win7-x64.exe` (595 KB).
## v0.4.0 — превью-основа экрана, JPEG, автоматизация горячих путей

- JPEG-DHT: счётчики кодов по длинам интерпретировались как per-symbol длины → новый `build_huff_from_counts`.
- IDCT: двойной level shift (+128 в IDCT и при записи в план) — убран из IDCT.
- `stream_of`: DCTDecode — терминальный фильтр, байты проходят сырыми (раньше Unsupported).
- Композиция матриц: `cm`/`Td`/`TD`/`T*` — `st.ctm.mul(&m)` (column-vector), `Tm` — замена.
- UI: превью — главная область; сайдбар 340 px (поиск/результаты/каталог/статистика).
- Фон-поток: `pipeline::scan_and_index` в `std::thread::spawn`, прогресс `Arc<Mutex<String>>`,
  перезагрузка каталога/индекса по флагу `reload_requested` в таймере (200 мс).
- Параллельная индексация: чанки по 8 потокам, локальные doc_id, слияние `Bm25::merge_chunk(part, offset)`.
  Ошибки суммируются по чанкам; join обязателен (регрессия: результаты дропались).
- Горячие пути (замеры): скан 360 файлов — 7–37 мс; индекс 120 PDF — 51–819 мс (холодный/тёплый);
  BM25-запрос — 59–72 мкс; SIMD AVX2/SSE2 в ядре BM25 (без изменений).

