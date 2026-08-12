# Exec document helpers

These network-free Python command-line scripts turn documents already present
in an exec workspace into concise stdout summaries and bounded images under
`preview/`.

| Script | Purpose | Required tooling |
| --- | --- | --- |
| `render_pdf.py` | Render selected PDF pages and an overview grid | Python, Pillow, and either pypdfium2 or pdf2image + Poppler |
| `extract_pdf_figures.py` | Extract embedded PDF raster figures and an overview | Python, Pillow, and Poppler `pdfimages` |
| `render_office.py` | Convert DOCX/PPTX to PDF, then render selected pages | The PDF renderer above, plus LibreOffice in the sandbox or a host-converted PDF under `.tidebreak/render/` |
| `analyze_xlsx.py` | Print sheet inventory, used ranges, and sample rows; thumbnail sheets | Python, openpyxl, and Pillow |
| `office_unpack.py` | Unzip an OOXML package into a directory tree for direct XML edits | Python |
| `pptx_clean.py` | Check an unpacked PPTX tree for malformed XML and dangling relationships | Python |
| `docx_clean.py` | Check an unpacked DOCX tree for malformed XML and dangling relationships | Python |
| `office_pack.py` | Zip an unpacked tree back into a valid OOXML file | Python |
| `calc_uno.py` | Inspect and edit an existing spreadsheet in place (cells, formulas, named ranges) | LibreOffice Calc plus its Python bridge (`python3-uno`) in the sandbox |
| `xlsx_recalc.py` | Recalculate a workbook and report error cells and numbers stored as text | LibreOffice Calc plus `python3-uno` in the sandbox |

`calc_uno.py` and `xlsx_recalc.py` share their soffice/UNO session plumbing in
`_tidebreak_calc.py`, and both must run under the system `python3` — that is the
interpreter LibreOffice's `uno` module is installed for.

The desktop copies this directory into each exec workspace at
`.tidebreak/exec-scripts`. The sandbox image exposes it at
`/opt/tidebreak/exec-scripts` and sets `TIDEBREAK_EXEC_SCRIPTS` to that path.
