"""A local stand-in for upload targets, used by upload-check.sh.

Accepts PUT on any path and appends one JSON line per request to LOG: method, path,
headers (lowercased), body length, and body sha256. Paths under /api/blob answer like the
Vercel Blob API; paths under /deny answer 403. Prints the bound port on stdout.
"""

import hashlib
import json
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlsplit

LOG = sys.argv[1]


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def do_PUT(self):
        length = int(self.headers.get("content-length", "0"))
        body = self.rfile.read(length)
        with open(LOG, "a") as f:
            f.write(json.dumps({
                "method": self.command,
                "path": self.path,
                "headers": {k.lower(): v for k, v in self.headers.items()},
                "length": len(body),
                "sha256": hashlib.sha256(body).hexdigest(),
            }) + "\n")
        url = urlsplit(self.path)
        if url.path.startswith("/deny"):
            return self.reply(403, b"<Error><Code>AccessDenied</Code><Message>Request has expired</Message></Error>")
        if url.path.startswith("/api/blob"):
            pathname = parse_qs(url.query)["pathname"][0]
            public = "https://store1.public.blob.vercel-storage.com/" + pathname
            return self.reply(200, json.dumps({
                "url": public,
                "downloadUrl": public + "?download=1",
                "pathname": pathname,
                "contentType": self.headers.get("x-content-type"),
                "contentDisposition": "inline",
                "etag": "\"mock\"",
            }).encode())
        self.reply(200, b"")

    def reply(self, status, body):
        self.send_response(status)
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass


server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
print(server.server_address[1], flush=True)
server.serve_forever()
