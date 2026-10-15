# `fixtures/net/`

Compressed answers for the HTTP client's tests (D-094): ureq unpacks gzip, and
the client caps what a body unpacks to. Both can be made again with the command
below (`mtime=0`, so the bytes don't change).

| File | What for |
|---|---|
| `answer.json.gz` | a small JSON answer sent gzip-encoded: the client asks for it and hands on the unpacked text |
| `zeros-1mib.gz` | 1 MiB of zero bytes in 1 KB; sent many times over as one body (gzip members follow each other), it unpacks past the cap while staying small on the wire |

```sh
python3 -c 'import gzip
open("answer.json.gz", "wb").write(gzip.compress(b"{\"contents\":\"unpacked by the client\"}", compresslevel=9, mtime=0))
open("zeros-1mib.gz", "wb").write(gzip.compress(b"\0" * 1048576, compresslevel=9, mtime=0))'
```
