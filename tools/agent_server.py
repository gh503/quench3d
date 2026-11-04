#!/usr/bin/env python3
"""Serve Quench3D Web and a same-origin OpenAI-compatible model proxy.

Run: QUENCH3D_LLM_API_KEY=... python3 tools/agent_server.py
Optional server environment: QUENCH3D_LLM_BASE_URL (API root or full chat endpoint),
QUENCH3D_LLM_MODEL, QUENCH3D_LLM_TIMEOUT (seconds). Credentials stay here.
Loopback providers may omit the key. This development server binds only to loopback.
"""
import argparse
import http.server
import json
import os
import socket
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass
from functools import partial
from pathlib import Path

MAX_BODY = 1024 * 1024


@dataclass(frozen=True)
class Config:
    base_url: str = "https://api.openai.com/v1"
    model: str = "gpt-4o-mini"
    api_key: str = ""
    timeout: float = 30.0

    @classmethod
    def from_env(cls):
        return cls(base_url=os.environ.get('QUENCH3D_LLM_BASE_URL', cls.base_url),
                   model=os.environ.get('QUENCH3D_LLM_MODEL', cls.model),
                   api_key=os.environ.get('QUENCH3D_LLM_API_KEY', ''),
                   timeout=float(os.environ.get('QUENCH3D_LLM_TIMEOUT', '30')))


class NoRedirect(urllib.request.HTTPRedirectHandler):
    # Never forward server credentials to a redirect destination.
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def create_server(host="127.0.0.1", port=8765, web_root=None, config=None):
    if host not in ('127.0.0.1', 'localhost'):
        raise ValueError('Development server supports only 127.0.0.1 or localhost binding')
    root = Path(web_root or Path(__file__).resolve().parent.parent / 'web').resolve()
    config = config or Config.from_env()
    endpoint = urllib.parse.urlsplit(config.base_url)
    if (endpoint.scheme not in ('http', 'https') or not endpoint.hostname
            or endpoint.username or endpoint.password or endpoint.query or endpoint.fragment
            or not 0 < config.timeout <= 300):
        raise ValueError('Invalid server model configuration')
    available = bool(config.api_key) or endpoint.hostname in ('localhost', '127.0.0.1', '::1')
    chat_url = config.base_url.rstrip('/')
    if not chat_url.endswith('/chat/completions'):
        chat_url += '/chat/completions'
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())

    class Handler(http.server.SimpleHTTPRequestHandler):
        def setup(self):
            super().setup()
            self.connection.settimeout(10)

        def log_message(self, *_args):
            # Do not log request bodies, headers, or upstream errors containing credentials.
            pass

        def reply(self, status, value):
            data = json.dumps(value, ensure_ascii=False).encode('utf8')
            self.send_response(status)
            self.send_header('Content-Type', 'application/json; charset=utf-8')
            self.send_header('Content-Length', str(len(data)))
            self.send_header('Cache-Control', 'no-store')
            self.send_header('X-Content-Type-Options', 'nosniff')
            self.end_headers()
            self.wfile.write(data)

        def error(self, status, message):
            self.reply(status, {'error': message})

        def same_origin(self):
            address, actual_port = self.server.server_address[:2]
            allowed = {f'{address}:{actual_port}'}
            if address == '127.0.0.1':
                allowed.add(f'localhost:{actual_port}')
            authority = self.headers.get('Host', '')
            origin = self.headers.get('Origin')
            return (authority in allowed
                    and (origin is None or origin == f'http://{authority}')
                    and self.headers.get('Sec-Fetch-Site', '').lower() not in ('cross-site', 'same-site'))

        def do_GET(self):
            if not self.same_origin():
                self.error(403, 'Same-origin requests only')
                return
            route = urllib.parse.urlsplit(self.path).path
            if route == '/api/agent/config':
                self.reply(200, {'available': available, 'model': config.model})
            elif route.startswith('/api/'):
                self.error(404, 'Unknown endpoint')
            else:
                super().do_GET()

        def do_HEAD(self):
            if not self.same_origin():
                self.send_error(403)
                return
            super().do_HEAD()

        def translate_path(self, path):
            translated = Path(super().translate_path(path)).resolve()
            # Includes symlink escapes; never expose files outside the configured Web root.
            return str(translated) if translated.is_relative_to(root) else str(root / '.forbidden')

        def list_directory(self, path):
            self.send_error(403, 'Directory listing disabled')
            return None

        def do_POST(self):
            self.close_connection = True
            if not self.same_origin():
                self.error(403, 'Same-origin requests only')
                return
            if self.path != '/api/agent/chat':
                self.error(404, 'Unknown endpoint')
                return
            if self.headers.get('Transfer-Encoding'):
                self.error(400, 'Transfer encoding unsupported')
                return
            try:
                length = int(self.headers.get('Content-Length', '-1'))
            except ValueError:
                length = -1
            if length < 0:
                self.error(400, 'Content-Length required')
                return
            if length > MAX_BODY:
                self.error(413, 'Request body too large')
                return
            if self.headers.get_content_type() != 'application/json':
                self.error(415, 'Expected application/json')
                return
            try:
                data = self.rfile.read(length)
                if len(data) != length:
                    raise ValueError()
                payload = json.loads(data)
                if (not isinstance(payload, dict) or set(payload) - {'messages', 'tools', 'model'}
                        or not isinstance(payload.get('messages'), list)
                        or not isinstance(payload.get('tools'), list)
                        or ('model' in payload and (not isinstance(payload['model'], str) or not payload['model']))):
                    raise ValueError()
            except (ValueError, UnicodeError, socket.timeout):
                self.error(400, 'Invalid chat request')
                return
            if not available:
                self.error(503, 'Model provider unavailable')
                return
            body = dict(payload, model=payload.get('model', config.model), stream=False)
            headers = {'Content-Type': 'application/json'}
            if config.api_key:
                headers['Authorization'] = f'Bearer {config.api_key}'
            request = urllib.request.Request(chat_url,
                data=json.dumps(body).encode('utf8'), headers=headers, method='POST')
            try:
                with opener.open(request, timeout=config.timeout) as response:
                    result = json.load(response)
                if not isinstance(result, dict):
                    raise ValueError()
            except (TimeoutError, socket.timeout):
                self.error(504, 'Model provider timed out')
                return
            except (urllib.error.URLError, ValueError, UnicodeError, OSError):
                self.error(502, 'Model provider request failed')
                return
            self.reply(200, result)

    return http.server.ThreadingHTTPServer((host, port), partial(Handler, directory=str(root)))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--host', choices=('127.0.0.1', 'localhost'), default='127.0.0.1')
    parser.add_argument('--port', type=int, default=8765)
    args = parser.parse_args()
    server = create_server(host=args.host, port=args.port)
    print(f'Quench3D editor: http://{args.host}:{server.server_port}', flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()


if __name__ == '__main__':
    main()
