"""Dev server for tiles/, which trunk forwards /tiles/ to (compose.yaml's
tiles service).

Python's http.server, made quiet. The map cancels tile requests it no
longer needs as it zooms and pans, which http.server reports as a
BrokenPipeError traceback each time; and a line per tile buries
everything else. This logs only failed requests.

    python3 tools/dev/tiles_server.py [port] [directory]
"""

import functools
import sys
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer

# The client went away mid-response: a cancelled request, not a fault.
GONE = (BrokenPipeError, ConnectionResetError)


class Handler(SimpleHTTPRequestHandler):
    def log_request(self, code="-", size="-"):
        if str(code)[:1] in "45":
            super().log_request(code, size)


class Server(ThreadingHTTPServer):
    daemon_threads = True

    def handle_error(self, request, client_address):
        if not isinstance(sys.exc_info()[1], GONE):
            super().handle_error(request, client_address)


def main():
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8081
    directory = sys.argv[2] if len(sys.argv) > 2 else "tiles"
    handler = functools.partial(Handler, directory=directory)
    print(f"tiles: serving {directory}/ on 127.0.0.1:{port}", flush=True)
    try:
        Server(("127.0.0.1", port), handler).serve_forever()
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
