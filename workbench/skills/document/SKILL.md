---
name: document
description: Produce a deliverable document file (.docx or .pdf) from content - reports, specs, summaries - via markdown + pandoc. Use when the user needs a shareable file, not project docs.
argument-hint: "[what the document is, and target format]"
---

# Deliverable documents (markdown → pandoc → docx/pdf)

One honest toolchain: author clean markdown, convert with pandoc. No fake Office XML by hand.

## Procedure

1. Author the content as a standalone markdown file (scratch or requested path) with a pandoc metadata block:
   ```yaml
   ---
   title: "…"
   author: "…"
   date: "YYYY-MM-DD"
   ---
   ```
   Structure: title → 3-6 line executive summary → body sections → appendix for detail. Tables for enumerable facts; prose for reasoning.
2. Check the converter: `pandoc --version`.
   - **.docx**: `pandoc in.md -o out.docx` (add `--reference-doc=<template>.docx` if the user has a house template).
   - **.pdf**: needs a PDF engine - try `pandoc in.md -o out.pdf --pdf-engine=typst` first, then `weasyprint`/`wkhtmltopdf` via HTML, then LaTeX if installed.
3. **Verify the artifact**: file exists, nonzero size; for pdf run `pdfinfo` (or open page count); mention anything that didn't convert (e.g. raw HTML blocks).
4. If pandoc is absent: deliver the clean markdown, say exactly what to install (`pandoc` + `typst` are enough on Arch), and offer to finish after.

## Guardrails
- Content comes from real sources in the conversation/repo - for run/pipeline reports prefer `ritual report` output as the base when `.ritual/` exists.
- Anything secret-looking in content gets redacted before it enters a shareable file.
- Never claim a conversion succeeded without checking the output file.
