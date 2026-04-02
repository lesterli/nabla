#!/usr/bin/env python3
"""
Long-lived Docling sidecar for nabla-pdf-rag.

Protocol (JSON lines over stdin/stdout):
  1. On startup, loads Docling models, prints: {"status": "ready"}
  2. Reads requests:  {"pdf_path": "/path/to.pdf", "document_id": "doc-123"}
  3. Writes responses: {"document_id": "...", "title": "...", "page_count": N, "elements": [...], "error": null}
  4. Loops until stdin is closed.

Each element: {"kind": "paragraph"|"section_header"|"table"|..., "text": "...", "page_number": N, "level": null|N}

Requires: pip install docling
"""

import json
import sys
import traceback


def load_converter():
    """Load Docling DocumentConverter (heavy — models download on first run)."""
    from docling.document_converter import DocumentConverter
    converter = DocumentConverter()
    return converter


def convert_document(converter, pdf_path: str, document_id: str) -> dict:
    """Convert a single PDF and return a StructuredDocument-compatible dict."""
    try:
        result = converter.convert(pdf_path)
        doc = result.document

        elements = []
        page_count = 0

        # Walk the document body in reading order
        for item, _level in doc.iterate_items():
            kind = "paragraph"
            text = ""
            page_number = 1
            level = None

            # Get text
            if hasattr(item, "text"):
                text = item.text or ""

            # Get page number from provenance
            if hasattr(item, "prov") and item.prov:
                page_number = item.prov[0].page_no
                page_count = max(page_count, page_number)

            # Determine element kind from label
            label = getattr(item, "label", None)
            if label is not None:
                label_str = label.value if hasattr(label, "value") else str(label)
                label_lower = label_str.lower()

                if label_lower == "title":
                    kind = "title"
                elif label_lower in ("section_header", "section-header"):
                    kind = "section_header"
                    # Try to get heading level
                    level = getattr(item, "level", None)
                    if level is None:
                        level = 1
                elif label_lower in ("paragraph", "text"):
                    kind = "paragraph"
                elif label_lower == "table":
                    kind = "table"
                    # Export table as markdown if possible
                    if hasattr(item, "export_to_markdown"):
                        text = item.export_to_markdown()
                elif label_lower == "list_item":
                    kind = "list_item"
                elif label_lower in ("picture", "figure"):
                    kind = "figure"
                    # Try to get caption
                    if hasattr(item, "captions") and item.captions:
                        cap_texts = []
                        for cap in item.captions:
                            if hasattr(cap, "text") and cap.text:
                                cap_texts.append(cap.text)
                        if cap_texts:
                            text = " ".join(cap_texts)
                elif label_lower == "code":
                    kind = "code"
                elif label_lower in ("formula", "equation"):
                    kind = "equation"
                elif label_lower == "page_header":
                    kind = "page_header"
                elif label_lower == "page_footer":
                    kind = "page_footer"

            if text:  # skip empty elements
                elements.append({
                    "kind": kind,
                    "text": text,
                    "page_number": page_number,
                    "level": level,
                })

        # Infer title
        title = None
        if hasattr(doc, "name") and doc.name:
            title = doc.name
        else:
            for elem in elements:
                if elem["kind"] == "title":
                    title = elem["text"]
                    break

        # Page count fallback
        if page_count == 0 and hasattr(doc, "pages"):
            page_count = len(doc.pages) if doc.pages else 1

        return {
            "document_id": document_id,
            "title": title,
            "page_count": page_count,
            "elements": elements,
            "error": None,
        }

    except Exception as e:
        return {
            "document_id": document_id,
            "title": None,
            "page_count": 0,
            "elements": [],
            "error": f"{type(e).__name__}: {e}",
        }


def main():
    # Redirect Docling's logging to stderr so it doesn't pollute the JSON protocol
    import logging
    logging.basicConfig(stream=sys.stderr, level=logging.WARNING)

    try:
        converter = load_converter()
    except ImportError:
        print(json.dumps({"status": "error", "message": "docling not installed. Run: pip install docling"}), flush=True)
        sys.exit(1)
    except Exception as e:
        print(json.dumps({"status": "error", "message": f"Failed to load Docling: {e}"}), flush=True)
        sys.exit(1)

    # Signal readiness
    print(json.dumps({"status": "ready"}), flush=True)

    # Process loop
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue

        try:
            req = json.loads(line)
        except json.JSONDecodeError as e:
            print(json.dumps({
                "document_id": "",
                "title": None,
                "page_count": 0,
                "elements": [],
                "error": f"Invalid JSON: {e}",
            }), flush=True)
            continue

        pdf_path = req.get("pdf_path", "")
        document_id = req.get("document_id", "")

        result = convert_document(converter, pdf_path, document_id)
        print(json.dumps(result, ensure_ascii=False), flush=True)


if __name__ == "__main__":
    main()
