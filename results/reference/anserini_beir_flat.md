# Anserini BEIR v1.0.0 BM25 "flat" reference numbers

Fetched 2026-11-13T06:33Z from the Anserini repo (master branch). Config: BM25 k1=0.9 b=0.4 (Anserini defaults), title+text concatenated, Lucene DefaultEnglishAnalyzer (Porter stemmer, Lucene stopwords), -removeQuery, -hits 1000, trec_eval -c.

| dataset | nDCG@10 | R@100 | R@1000 | documents | total terms (Lucene) | source |
|---|---|---|---|---|---|---|
| scifact | 0.6789 | 0.9253 | 0.9767 | 5183 | 838128 | https://github.com/castorini/anserini/blob/master/docs/reproduce/from-document-collection/beir-v1.0.0-scifact.flat.md |
| nfcorpus | 0.3218 | 0.2457 | 0.3704 | 3633 | 637485 | https://github.com/castorini/anserini/blob/master/docs/reproduce/from-document-collection/beir-v1.0.0-nfcorpus.flat.md |
| fiqa | 0.2361 | 0.5395 | 0.7393 | 57600 | 5288635 | https://github.com/castorini/anserini/blob/master/docs/reproduce/from-document-collection/beir-v1.0.0-fiqa.flat.md |
| scidocs | 0.1490 | 0.3477 | 0.5638 | 25657 | 3266767 | https://github.com/castorini/anserini/blob/master/docs/reproduce/from-document-collection/beir-v1.0.0-scidocs.flat.md |
| trec-covid | 0.5947 | 0.1091 | 0.3955 | 171331 | 20822821 | https://github.com/castorini/anserini/blob/master/docs/reproduce/from-document-collection/beir-v1.0.0-trec-covid.flat.md |
