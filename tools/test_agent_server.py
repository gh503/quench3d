"""Real HTTP proxy tests: all model requests stay on localhost."""
import http.client
import http.server
import json
import io
from contextlib import redirect_stderr
from pathlib import Path
import tempfile
import threading
import unittest

from agent_server import Config, create_server


class AgentServerTests(unittest.TestCase):
    def setUp(self):
        self.requests = []
        self.models_response = {'data': [{'id': 'model-b'}, {'id': 'model-a'}, {'id': 'model-b'}]}
        self.models_status = 200
        self.models_delay = 0
        owner = self

        class Upstream(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                owner.requests.append((self.path, dict(self.headers), None))
                if owner.models_delay:
                    threading.Event().wait(owner.models_delay)
                self.send_response(owner.models_status)
                if owner.models_status == 302:
                    self.send_header('Location', '/credentials-must-not-follow')
                data = (owner.models_response if isinstance(owner.models_response, bytes)
                        else json.dumps(owner.models_response).encode())
                self.send_header('Content-Type', 'application/json')
                self.send_header('Content-Length', str(len(data)))
                self.end_headers()
                try:
                    self.wfile.write(data)
                except (BrokenPipeError, ConnectionResetError):
                    pass

            def do_POST(self):
                body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
                owner.requests.append((self.path, dict(self.headers), body))
                response = {'choices': [{'message': {'role': 'assistant', 'content': None,
                    'tool_calls': [{'id': 'call_1', 'type': 'function', 'function': {
                        'name': 'inspect_scene', 'arguments': '{}'}}]}}]}
                if body['model'] == 'slow':
                    threading.Event().wait(0.2)
                if body['model'] == 'redirect':
                    self.send_response(302)
                    self.send_header('Location', '/credentials-must-not-follow')
                    self.send_header('Content-Length', '0')
                    self.end_headers()
                    return
                if body['model'] == 'error':
                    self.send_response(401)
                    data = b'upstream secret-test-key details'
                else:
                    self.send_response(200)
                    data = json.dumps(response).encode()
                self.send_header('Content-Type', 'application/json')
                self.send_header('Content-Length', str(len(data)))
                self.end_headers()
                try:
                    self.wfile.write(data)
                except BrokenPipeError:
                    pass

            def log_message(self, *_args):
                pass

        self.upstream = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Upstream)
        self.start(self.upstream)
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name) / 'web'
        self.root.mkdir()
        (self.root / 'index.html').write_text('editor-home', encoding='utf8')
        (Path(self.temp.name) / 'private.txt').write_text('private-data', encoding='utf8')
        self.config = Config(base_url=f'http://127.0.0.1:{self.upstream.server_port}/v1',
                             model='test-model', api_key='secret-test-key', timeout=1)
        self.server = create_server(port=0, web_root=self.root, config=self.config)
        self.start(self.server)
        self.origin = f'http://127.0.0.1:{self.server.server_port}'

    def start(self, server):
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        def stop():
            server.shutdown()
            server.server_close()
            thread.join(timeout=2)
        self.addCleanup(stop)

    def request(self, method='POST', path='/api/agent/chat', payload=None, headers=None):
        connection = http.client.HTTPConnection('127.0.0.1', self.server.server_port, timeout=3)
        self.addCleanup(connection.close)
        body = json.dumps(payload if payload is not None else {'messages': [], 'tools': []})
        actual_headers = {'Origin': self.origin, 'Content-Type': 'application/json'}
        actual_headers.update(headers or {})
        connection.request(method, path, body=body if method == 'POST' else None, headers=actual_headers)
        response = connection.getresponse()
        return response.status, response.read()

    def models_request(self, connection=None, **kwargs):
        if connection is None:
            connection = {'base_url': self.config.base_url, 'api_key': 'discovery-key', 'timeout_secs': 1}
        return self.request(path='/api/agent/models', payload={'connection': connection}, **kwargs)

    def test_models_uses_client_connection_and_normalizes_endpoint(self):
        local = create_server(port=0, web_root=self.root)
        self.start(local)
        self.server = local
        self.origin = f'http://127.0.0.1:{local.server_port}'
        for suffix in ['/v1', '/v1/', '/v1/chat/completions', '/v1/chat/completions/',
                       '/v1/models', '/v1/models/']:
            with self.subTest(suffix=suffix):
                status, data = self.models_request({'base_url': f'http://127.0.0.1:{self.upstream.server_port}{suffix}',
                                                   'api_key': 'discovery-key', 'timeout_secs': 1})
                self.assertEqual(status, 200)
                self.assertEqual(json.loads(data), {'models': ['model-b', 'model-a']})
                path, headers, body = self.requests[-1]
                self.assertEqual(path, '/v1/models')
                self.assertEqual(headers['Authorization'], 'Bearer discovery-key')
                self.assertIsNone(body)
                self.assertNotIn(b'discovery-key', data)
        self.assertEqual(self.models_request({'base_url': self.config.base_url, 'api_key': '', 'timeout_secs': 1})[0], 200)
        self.assertNotIn('Authorization', self.requests[-1][1])
        self.models_response = {'data': []}
        self.assertEqual(json.loads(self.models_request()[1]), {'models': []})

    def test_models_rejects_invalid_connections_without_upstream_requests(self):
        base = {'base_url': self.config.base_url, 'api_key': '', 'timeout_secs': 1}
        patches = [{'base_url': value} for value in ['file:///etc/passwd', 'https://user:pass@example.test/v1',
                   'http://127.0.0.1:invalid', 'http://[broken', 'https://example.test/v1?key=x',
                   'https://example.test/v1#fragment', 'https://example.test/\nheader']]
        patches += [{'api_key': value} for value in ['key\r\ninjected', '\u2603', None]]
        patches += [{'timeout_secs': value} for value in [0, 301, True, '1', float('nan'), float('inf'), 10 ** 400]]
        for patch in patches:
            with self.subTest(patch=patch):
                self.assertEqual(self.models_request(dict(base, **patch))[0], 400)
        for payload in [{}, [], {'connection': None}, {'connection': {}},
                        {'connection': base, 'api_key': 'extra'},
                        {'connection': dict(base, model='unneeded')}]:
            self.assertEqual(self.request(path='/api/agent/models', payload=payload)[0], 400)
        self.assertEqual(self.requests, [])
        self.assertEqual(self.models_request({'base_url': 'https://example.test/v1', 'api_key': '', 'timeout_secs': 1})[0], 503)
        self.assertEqual(self.requests, [])

    def test_models_enforces_origin_and_request_body_limits(self):
        for headers in [{'Origin': 'https://attacker.test'}, {'Origin': 'null'},
                        {'Sec-Fetch-Site': 'cross-site'}, {'Sec-Fetch-Site': 'same-site'}, {'Host': 'attacker.test'}]:
            self.assertEqual(self.models_request(headers=headers)[0], 403)
        self.assertEqual(self.request('GET', '/api/agent/models')[0], 404)
        self.assertEqual(self.models_request(headers={'Transfer-Encoding': 'chunked'})[0], 400)
        self.assertEqual(self.models_request(headers={'Content-Type': 'text/plain'})[0], 415)
        self.assertEqual(self.models_request(headers={'Content-Length': '-1'})[0], 400)
        # Declare the oversized body without racing a 1 MiB send against early close.
        self.assertEqual(self.models_request(headers={'Content-Length': str(1024 * 1024 + 1)})[0], 413)
        connection = http.client.HTTPConnection('127.0.0.1', self.server.server_port, timeout=3)
        self.addCleanup(connection.close)
        connection.request('POST', '/api/agent/models', body=b'{', headers={'Origin': self.origin, 'Content-Type': 'application/json'})
        self.assertEqual(connection.getresponse().status, 400)
        self.assertEqual(self.requests, [])

    def test_models_rejects_bad_upstream_payloads_and_hides_errors(self):
        bad_responses = [b'not-json discovery-key', [], {}, {'data': {}}, {'data': [None]},
                         {'data': [{'id': None}]}, {'data': [{'id': ''}]}, {'data': [{'id': 'x' * 257}]},
                         {'data': [{'id': 'bad\nmodel'}]}, b'x' * (1024 * 1024 + 1)]
        captured = io.StringIO()
        with redirect_stderr(captured):
            for response in bad_responses:
                self.models_response = response
                status, data = self.models_request()
                self.assertEqual(status, 502)
                self.assertNotIn(b'discovery-key', data)
            self.models_response = b'discovery-key upstream details'
            for status_code in [401, 500, 302]:
                self.models_status = status_code
                status, data = self.models_request()
                self.assertEqual(status, 502)
                self.assertNotIn(b'discovery-key', data)
                self.assertNotIn(b'upstream details', data)
        self.assertNotIn('discovery-key', captured.getvalue())
        self.assertFalse(any(r[0] == '/credentials-must-not-follow' for r in self.requests))
        self.models_status = 200
        self.models_delay = 0.2
        status, data = self.models_request({'base_url': self.config.base_url, 'api_key': 'discovery-key', 'timeout_secs': 0.03})
        self.assertEqual(status, 504)
        self.assertNotIn(b'discovery-key', data)

    def test_http_tool_response_preserves_null_and_forwards_exact_history_schema(self):
        messages = [{'role':'user','content':'inspect'}, {'role':'assistant','content':None,
            'tool_calls':[{'id':'old','type':'function','function':{'name':'inspect_scene','arguments':'{}'}}]},
            {'role':'tool','tool_call_id':'old','content':'{"objects":3}'}]
        tools = [{'type':'function','function':{'name':'inspect_scene','parameters':{'type':'object'}}}]
        status, data = self.request(payload={'messages':messages,'tools':tools})
        self.assertEqual(status, 200)
        reply = json.loads(data)
        self.assertIsNone(reply['choices'][0]['message']['content'])
        self.assertEqual(reply['choices'][0]['message']['tool_calls'][0]['id'], 'call_1')
        path, headers, body = self.requests[0]
        self.assertEqual(path, '/v1/chat/completions')
        self.assertEqual(headers['Authorization'], 'Bearer secret-test-key')
        self.assertEqual(body, {'messages':messages,'tools':tools,'model':'test-model','stream':False})
        self.assertNotIn(b'secret-test-key', data)

    def test_client_connection_overrides_server_and_forwards_model_options(self):
        connection = {'base_url': self.config.base_url, 'model': 'client-model',
                      'api_key': 'client-only-key', 'temperature': 0.35,
                      'max_tokens': 4096, 'timeout_secs': 2}
        status, data = self.request(payload={'messages': [], 'tools': [], 'connection': connection})
        self.assertEqual(status, 200)
        _, headers, body = self.requests[-1]
        self.assertEqual(headers['Authorization'], 'Bearer client-only-key')
        self.assertEqual(body['model'], 'client-model')
        self.assertEqual(body['temperature'], 0.35)
        self.assertEqual(body['max_tokens'], 4096)
        self.assertNotIn('connection', body)
        self.assertNotIn(b'client-only-key', data)

    def test_invalid_client_connection_does_not_contact_upstream(self):
        base = {'base_url': self.config.base_url, 'model': 'client-model', 'api_key': '',
                'temperature': 0.2, 'max_tokens': 2048, 'timeout_secs': 30}
        for patch in [{'base_url': 'file:///etc/passwd'}, {'api_key': 'key\r\ninjected'},
                      {'timeout_secs': 0}, {'temperature': float('nan')},
                      {'max_tokens': True}, {'base_url': 'https://user:pass@example.test/v1'}]:
            status, _ = self.request(payload={'messages': [], 'tools': [], 'connection': dict(base, **patch)})
            self.assertEqual(status, 400)
        self.assertEqual(self.requests, [])

    def test_default_server_requires_client_configuration(self):
        local = create_server(port=0, web_root=self.root)
        self.start(local)
        self.server = local
        self.origin = f'http://127.0.0.1:{local.server_port}'
        status, data = self.request('GET', '/api/agent/config')
        self.assertEqual(status, 200)
        self.assertEqual(json.loads(data)['configuration'], 'client')
        self.assertEqual(self.request()[0], 400)

    def test_config_and_static_root_never_expose_key_or_parent(self):
        status, data = self.request('GET', '/api/agent/config')
        self.assertEqual(status, 200)
        self.assertEqual(json.loads(data), {'available':True,'model':'test-model'})
        self.assertNotIn(b'secret-test-key', data)
        self.assertEqual(self.request('GET', '/')[1], b'editor-home')
        (self.root / 'escape.txt').symlink_to(Path(self.temp.name) / 'private.txt')
        for path in ['/../private.txt', '/%2e%2e/private.txt', '/escape.txt']:
            status, data = self.request('GET', path)
            self.assertIn(status, (403,404))
            self.assertNotIn(b'private-data', data)

    def test_rejects_cross_origin_and_cross_site_without_contacting_model(self):
        for headers in [{'Origin':'https://attacker.test'}, {'Origin':'null'},
                        {'Sec-Fetch-Site':'cross-site'}, {'Host':'attacker.test'}]:
            self.assertEqual(self.request(headers=headers)[0], 403)
        self.assertEqual(self.requests, [])

    def test_client_cannot_select_upstream_authorization_or_stream(self):
        for key in ['base_url','api_key','authorization','stream']:
            self.assertEqual(self.request(payload={'messages':[],'tools':[],key:'malicious'})[0], 400)
        self.assertEqual(self.requests, [])

    def test_body_limit_and_safe_upstream_errors(self):
        self.assertEqual(self.request(payload={'messages':[{'role':'user','content':'x' * (1024*1024)}],'tools':[]})[0], 413)
        status, data = self.request(payload={'messages':[],'tools':[],'model':'error'})
        self.assertEqual(status, 502)
        self.assertNotIn(b'secret-test-key', data)
        self.assertNotIn(b'upstream secret', data)

    def test_local_provider_needs_no_key_and_full_endpoint_is_idempotent(self):
        for suffix in ['/v1', '/v1/chat/completions', '/v1/chat/completions/']:
            local = create_server(port=0, web_root=self.root, config=Config(
                base_url=f'http://127.0.0.1:{self.upstream.server_port}{suffix}', model='local-model'))
            self.start(local)
            self.server = local
            self.origin = f'http://127.0.0.1:{local.server_port}'
            status, data = self.request()
            self.assertEqual(status, 200, 'configured loopback provider must work without a key')
            self.assertIsNone(json.loads(data)['choices'][0]['message']['content'])
            self.assertEqual(json.loads(self.request('GET', '/api/agent/config')[1])['available'], True)
            path, headers, _body = self.requests[-1]
            self.assertEqual(path, '/v1/chat/completions')
            self.assertNotIn('Authorization', headers)

    def test_server_rejects_non_loopback_bind(self):
        with self.assertRaises(ValueError):
            create_server(host='0.0.0.0', port=0, config=self.config)

    def test_timeout_redirect_and_unconfigured_provider_are_safe(self):
        short = create_server(port=0, web_root=self.root, config=Config(
            base_url=self.config.base_url, model='test-model', api_key='secret-test-key', timeout=0.03))
        self.start(short)
        original_server, original_origin = self.server, self.origin
        self.server = short
        self.origin = f'http://127.0.0.1:{short.server_port}'
        status, data = self.request(payload={'messages':[], 'tools':[], 'model':'slow'})
        self.assertEqual(status, 504)
        self.assertNotIn(b'secret-test-key', data)
        self.server, self.origin = original_server, original_origin
        self.assertEqual(self.request(payload={'messages':[], 'tools':[], 'model':'redirect'})[0], 502)
        self.assertFalse(any(r[0] == '/credentials-must-not-follow' for r in self.requests))
        unavailable = create_server(port=0, web_root=self.root, config=Config(api_key=''))
        self.start(unavailable)
        self.server = unavailable
        self.origin = f'http://127.0.0.1:{unavailable.server_port}'
        self.assertEqual(json.loads(self.request('GET', '/api/agent/config')[1])['available'], False)
        self.assertEqual(self.request()[0], 503)


if __name__ == '__main__':
    unittest.main()
