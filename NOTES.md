# pdfcat v0.2.0 — окно Bento Grids

## Русский
**Первый релиз с GUI.** Запуск без аргументов открывает окно Bento на GDI (тёмная тема, сетка 8 px):
- плитки: «Каталог» (**дерево и граф** — оба режима), «Результаты поиска» с вводом и каретой,
  «Статистика», «Превью текста»;
- «Сканировать…» → выбор папки → скан + BM25-индексация; **сетевой диск не изменяется ни на байт**;
- двойной клик по PDF — открыть в просмотрщике (тоже только чтение);
- БД рядом с exe (портативно), при отказе прав — %LOCALAPPDATA%\pdfcat;
- OCR не существует по решению владельца.

CLI-команды прежние: scan / index / search / stats / peek / snapshot.
Win7: `pdfcat-0.2.0-win7-x64.exe` (KB2999226 нужен на целевой машине), 595 КБ.
Тесты 17/17. Прочие ассеты: `pdfcat-0.2.0-win64.exe`, SHA256SUMS.txt.

## English
**First GUI release.** Running without arguments opens the Bento window on GDI (dark theme, 8px grid):
Catalog tile with **both tree and graph modes**, Results tile with search input and caret,
Stats, Text preview. "Scan…" → folder picker → scan + BM25 index; **the scanned drive is never
written to**. Double-click opens the PDF read-only. DB stored next to the exe (portable), with a
%LOCALAPPDATA%\pdfcat fallback. No OCR, by owner decision (scans are matched by name only).

CLI unchanged: scan / index / search / stats / peek / snapshot.
Win7 asset: `pdfcat-0.2.0-win7-x64.exe` (requires KB2999226 on the target), 595 KB. Tests 17/17.
