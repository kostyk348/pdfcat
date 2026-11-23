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
