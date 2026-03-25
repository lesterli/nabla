#!/usr/bin/env python3
"""Mock sidecar for testing — returns fake parsed content."""
import json
import sys

req = json.loads(sys.stdin.readline())
resp = {
    "document_id": req["document_id"],
    "inferred_title": "Mock Research Paper",
    "pages": [
        {"page_number": 1, "text": "Abstract This paper investigates the effects of hierarchical summarization on retrieval quality in RAG systems."},
        {"page_number": 2, "text": "Introduction Recent advances in large language models have enabled new approaches to document understanding and retrieval."},
        {"page_number": 3, "text": "Methods We propose RAPTOR-lite a simplified hierarchical summary tree that balances retrieval quality with computational cost."},
        {"page_number": 4, "text": "Results Our evaluation shows that hierarchical summaries improve recall by 23% compared to flat chunk retrieval."},
        {"page_number": 5, "text": "Conclusion Hierarchical document structure preservation is essential for accurate citation and evidence tracing in research applications."},
    ],
    "error": None,
}
print(json.dumps(resp))
