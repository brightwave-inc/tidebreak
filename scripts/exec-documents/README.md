# Exec document helpers

These network-free Python command-line scripts turn documents already present
in an exec workspace into concise stdout summaries and bounded images under
`preview/`.

| Script | Purpose | Required tooling |
| --- | --- | --- |
| `render_pdf.py` | Render selected PDF pages and an overview grid | Python, Pillow, and either pypdfium2 or pdf2image + Poppler |
| `extract_pdf_figures.py` | Extract embedded PDF raster figures and an overview | Python, Pillow, and Poppler `pdfimages` |
| `render_office.py` | Convert DOCX/PPTX to PDF, then render selected pages | The PDF renderer above plus LibreOffice |
| `analyze_xlsx.py` | Print sheet inventory, used ranges, and sample rows; thumbnail sheets | Python, openpyxl, and Pillow |

The desktop copies this directory into each exec workspace at
`.openwave/exec-scripts`. The sandbox image exposes it at
`/opt/openwave/exec-scripts` and sets `OPENWAVE_EXEC_SCRIPTS` to that path.
