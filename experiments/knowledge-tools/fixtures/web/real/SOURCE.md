# Source

Each JSON file here holds the text blocks `search_web`'s extractor took from one public web
page, fetched on 2026-10-15 with the pond's own user agent by `freeze_live_pages` in
`crates/pond-mcp-server/src/web_search.rs` (branch `feat/web-search-backend`). Its `url` field is
the page it came from and `fetched` the date. Nothing else from the page is kept: no markup,
images, scripts or links.

Each page belongs to its publisher:

| file | publisher |
|---|---|
| `wikipedia-sgr.json`, `wikipedia-jkia.json` | Wikipedia contributors, under [CC BY-SA 4.0](https://creativecommons.org/licenses/by-sa/4.0/) |
| `parliament-national-assembly.json` | Parliament of Kenya |
| `bbc-sgr-opening.json` | BBC |
| `govuk-kenya-entry.json` | UK Foreign, Commonwealth & Development Office (GOV.UK, Open Government Licence) |
| `kws-nairobi-national-park.json` | Kenya Wildlife Service |
| `kenyatronics-iphone.json` | Kenyatronics |
| `standard-sgr-two-million.json` | The Standard Group |
| `immigration-passport-fees.json`, `immigration-faq.json` | Directorate of Immigration Services, Kenya |

The text is kept unmodified, as frozen test input, so that runs compare passage selection and
not the network. It is not Brave output: the bench never stores search results (Brave's terms,
3(b)(i) and 3(b)(xiii)).
