#!/usr/bin/env python3
"""Serve Quench3D Web and a same-origin OpenAI-compatible model proxy.

Run: python3 tools/agent_server.py, then configure providers/models in the editor.
Each request supplies the locally saved client connection; the transport does not
persist credentials or select a model from server environment variables.
Loopback providers may omit the key. This server binds only to loopback.
"""
import argparse
import http.client
import http.server
import json
import math
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


def validate_connection(base_url, model, api_key, timeout):
    if not all(isinstance(v, str) for v in (base_url, model, api_key)):
        raise ValueError('Invalid client connection')
    if any(ord(c) < 32 or ord(c) == 127 for v in (base_url, model, api_key) for c in v):
        raise ValueError('Invalid client connection')
    endpoint = urllib.parse.urlsplit(base_url)
    if (endpoint.scheme not in ('http', 'https') or not endpoint.hostname
            or endpoint.username or endpoint.password or endpoint.query or endpoint.fragment
            or not model.strip() or len(model) > 256 or len(api_key) > 8192
            or isinstance(timeout, bool) or not isinstance(timeout, (int, float))
            or not math.isfinite(timeout) or not 0 < timeout <= 300):
        raise ValueError('Invalid client connection')
    # Accessing port also validates malformed/non-numeric port values.
    endpoint.port
    url = base_url.rstrip('/')
    if not url.endswith('/chat/completions'):
        url += '/chat/completions'
    return url, bool(api_key) or endpoint.hostname in ('localhost', '127.0.0.1', '::1')


class NoRedirect(urllib.request.HTTPRedirectHandler):
    # Never forward server credentials to a redirect destination.
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def create_server(host="127.0.0.1", port=8765, web_root=None, config=None):
    if host not in ('127.0.0.1', 'localhost'):
        raise ValueError('Development server supports only 127.0.0.1 or localhost binding')
    root = Path(web_root or Path(__file__).resolve().parent.parent / 'web').resolve()
    # Normal application mode is entirely client configured. Explicit config is
    # retained for deterministic embedded fixtures, never read from server env.
    if config is not None:
        validate_connection(config.base_url, config.model, config.api_key, config.timeout)
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
                if config is None:
                    self.reply(200, {'configuration': 'client', 'available': True})
                else:
                    _, available = validate_connection(config.base_url, config.model, config.api_key, config.timeout)
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

        def discover_models(self, payload):
            try:
                if not isinstance(payload, dict) or set(payload) != {'connection'}:
                    raise ValueError()
                connection = payload['connection']
                if not isinstance(connection, dict) or set(connection) != {'base_url', 'api_key', 'timeout_secs'}:
                    raise ValueError()
                base_url, api_key = connection['base_url'], connection['api_key']
                timeout = connection['timeout_secs']
                # Share chat's URL, credential and timeout validation without
                # requiring a selected model in order to discover one.
                chat_url, available = validate_connection(base_url, 'discovery', api_key, timeout)
                models_url = chat_url.removesuffix('/chat/completions')
                if not models_url.endswith('/models'):
                    models_url += '/models'
                # urllib encodes headers as latin-1; reject unusable keys safely.
                api_key.encode('latin-1')
            except (ValueError, TypeError, UnicodeError, OverflowError):
                self.error(400, 'Configure a valid provider in the local client settings')
                return
            if not available:
                self.error(503, 'Model provider unavailable')
                return
            headers = {'Accept': 'application/json'}
            if api_key:
                headers['Authorization'] = f'Bearer {api_key}'
            try:
                request = urllib.request.Request(models_url, headers=headers, method='GET')
                with opener.open(request, timeout=timeout) as response:
                    data = response.read(MAX_BODY + 1)
                if len(data) > MAX_BODY:
                    raise ValueError()
                result = json.loads(data)
                if not isinstance(result, dict) or not isinstance(result.get('data'), list):
                    raise ValueError()
                models = []
                seen = set()
                for entry in result['data']:
                    if not isinstance(entry, dict):
                        raise ValueError()
                    model = entry.get('id')
                    if (not isinstance(model, str) or not model.strip() or len(model) > 256
                            or any(ord(c) < 32 or ord(c) == 127 for c in model)):
                        raise ValueError()
                    if model not in seen:
                        models.append(model)
                        seen.add(model)
            except (TimeoutError, socket.timeout):
                self.error(504, 'Model provider timed out')
                return
            except (urllib.error.URLError, ValueError, UnicodeError, OSError, http.client.HTTPException):
                self.error(502, 'Model provider request failed')
                return
            self.reply(200, {'models': models})

        def do_POST(self):
            self.close_connection = True
            if not self.same_origin():
                self.error(403, 'Same-origin requests only')
                return
            if self.path not in ('/api/agent/chat', '/api/agent/models'):
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
                if self.path == '/api/agent/models':
                    self.discover_models(payload)
                    return
                if (not isinstance(payload, dict) or set(payload) - {'messages', 'tools', 'model', 'connection'}
                        or not isinstance(payload.get('messages'), list)
                        or not isinstance(payload.get('tools'), list)
                        or ('model' in payload and (not isinstance(payload['model'], str) or not payload['model']))):
                    raise ValueError()
            except (ValueError, UnicodeError, socket.timeout):
                self.error(400, 'Invalid models request' if self.path == '/api/agent/models' else 'Invalid chat request')
                return
            try:
                connection = payload.get('connection')
                options = {}
                if connection is not None:
                    if (not isinstance(connection, dict) or set(connection) != {
                            'base_url', 'model', 'api_key', 'temperature', 'max_tokens', 'timeout_secs'}):
                        raise ValueError()
                    base_url, model = connection['base_url'], connection['model']
                    api_key, timeout = connection['api_key'], connection['timeout_secs']
                    temperature, max_tokens = connection['temperature'], connection['max_tokens']
                    if (isinstance(temperature, bool) or not isinstance(temperature, (int, float))
                            or not math.isfinite(temperature) or not 0 <= temperature <= 2
                            or isinstance(max_tokens, bool) or not isinstance(max_tokens, int)
                            or not 1 <= max_tokens <= 1048576):
                        raise ValueError()
                    options = {'temperature': temperature, 'max_tokens': max_tokens}
                elif config is not None:
                    base_url, model = config.base_url, payload.get('model', config.model)
                    api_key, timeout = config.api_key, config.timeout
                else:
                    raise ValueError()
                chat_url, available = validate_connection(base_url, model, api_key, timeout)
            except (ValueError, TypeError):
                self.error(400, 'Configure a valid model in the local client settings')
                return
            if not available:
                self.error(503, 'Model provider unavailable')
                return
            body = dict(messages=payload['messages'], tools=payload['tools'], model=model, stream=False, **options)
            headers = {'Content-Type': 'application/json'}
            if api_key:
                headers['Authorization'] = f'Bearer {api_key}'
            request = urllib.request.Request(chat_url,
                data=json.dumps(body).encode('utf8'), headers=headers, method='POST')
            try:
                with opener.open(request, timeout=timeout) as response:
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
