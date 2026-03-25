#!/usr/bin/env python3
"""
Docling PDF parser sidecar for nabla-pdf-rag.

Protocol: reads one JSON line from stdin, writes one JSON line to stdout.

Input:  {"pdf_path": "/path/to/file.pdf", "document_id": "doc-123"}
Output: {"document_id": "doc-123", "inferred_title": "...", "pages": [...], "error": null}

Requires: pip install docling
"""
import json
import sys


def parse_with_docling(pdf_path: str, document_id: str) -> dict:
    try:
        from docling.document_converter import DocumentConverter

        converter = DocumentConverter()
        result = converter.convert(pdf_path)

        doc = result.document
        pages = []
        # Docling provides text per page via the export
        md_text = doc.export_to_markdown()

        # Fallback: if page-level text is available, use it;
        # otherwise treat the whole markdown as page 1.
        # Docling's internal page segmentation depends on the PDF structure.
        if hasattr(doc, "pages") and doc.pages:
            for i, page in enumerate(doc.pages):
                page_text = ""
                if hasattr(page, "text"):
                    page_text = page.text
                elif hasattr(page, "cells"):
                    page_text = "\n".join(
                        c.text for c in page.cells if hasattr(c, "text")
                    )
                pages.append({"page_number": i + 1, "text": page_text})
        else:
            pages.append({"page_number": 1, "text": md_text})

        inferred_title = None
        if hasattr(doc, "title") and doc.title:
            inferred_title = doc.title
        elif pages and pages[0]["text"]:
            # Use first non-empty line as title heuristic
            for line in pages[0]["text"].split("\n"):
                stripped = line.strip().lstrip("#").strip()
                if stripped:
                    inferred_title = stripped[:200]
                    break

        return {
            "document_id": document_id,
            "inferred_title": inferred_title,
            "pages": pages,
            "error": None,
        }

    except ImportError:
        return {
            "document_id": document_id,
            "inferred_title": None,
            "pages": [],
            "error": "docling not installed. Run: pip install docling",
        }
    except Exception as e:
        return {
            "document_id": document_id,
            "inferred_title": None,
            "pages": [],
            "error": str(e),
        }


def main():
    line = sys.stdin.readline().strip()
    if not line:
        print(json.dumps({"document_id": "", "inferred_title": None, "pages": [], "error": "empty input"}))
        return

    req = json.loads(line)
    result = parse_with_docling(req["pdf_path"], req["document_id"])
    print(json.dumps(result))


if __name__ == "__main__":
    main()
