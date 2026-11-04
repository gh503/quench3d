#!/usr/bin/env python3
"""Loopback-only deterministic model fixture for probe_web_agent.mjs.

Without arguments run pure fixture unit tests (no listening sockets). --serve
starts agent_server's same-origin Web/proxy and a fake model on random loopback
ports, prints one JSON readiness line, and stays alive until stdin closes.
No environment credentials or provider URL are read. The compatibility flag
late_sent records completion of the upstream send attempt, not browser delivery;
cancellation may abort the connection before the attempted response arrives.
"""
import argparse
import http.server
import json
from pathlib import Path
import sys
import threading
import unittest

from agent_server import Config, create_server


class ModelScript:
    STEPS = ('set_playback', 'inspect_scene', 'spawn_primitive', 'set_transform',
             'inspect_scene', 'undo', 'inspect_scene', 'redo', 'inspect_scene')

    def __init__(self):
        self.lock = threading.Lock()
        self.release = threading.Event()
        self.delayed = threading.Event()
        self.requests = []
        self.errors = []
        self.completed = False
        self.cancel_requests = 0
        self.late_sent = threading.Event()

    def report(self):
        with self.lock:
            return {'requests': self.requests.copy(), 'errors': self.errors.copy(),
                    'completed': self.completed, 'delayed': self.delayed.is_set(),
                    'cancel_requests': self.cancel_requests, 'late_sent': self.late_sent.is_set()}

    @staticmethod
    def result(body, call_id):
        matches = [m for m in body['messages'] if m.get('role') == 'tool'
                   and m.get('tool_call_id') == call_id]
        if len(matches) != 1:
            raise AssertionError(f'expected exactly one tool result for {call_id}')
        value = json.loads(matches[0]['content'])
        if value.get('ok') is not True or not isinstance(value.get('revision'), str) or not value['revision']:
            raise AssertionError(f'unsuccessful or revisionless tool result: {value}')
        return value

    @staticmethod
    def response(message):
        return {'choices': [{'message': dict(role='assistant', **message)}]}

    @classmethod
    def call(cls, index, name, args):
        return cls.response({'content': None, 'tool_calls': [{
            'id': f'fixture_{index}', 'type': 'function',
            'function': {'name': name, 'arguments': json.dumps(args, ensure_ascii=False)}}]})

    def reply(self, body):
        prompts = [m.get('content', '') for m in body['messages'] if m.get('role') == 'user']
        if any('fixture-cancel' in str(p) for p in prompts):
            with self.lock:
                self.cancel_requests += 1
            self.delayed.set()
            if not self.release.wait(20):
                raise AssertionError('cancel response was never released by probe')
            cancel_prompt = next(str(p) for p in prompts if 'fixture-cancel:' in str(p))
            revision = json.loads(cancel_prompt.split('fixture-cancel:', 1)[1])['revision']
            if not isinstance(revision, str) or not revision:
                raise AssertionError('cancel fixture needs a valid actual scene revision')
            return self.call('late', 'spawn_primitive', {'name': '取消后不应出现',
                'shape': {'type': 'sphere', 'radius': 0.4}, 'expected_revision': revision})
        tool_messages = [m for m in body['messages'] if m.get('role') == 'tool']
        index = len(tool_messages)
        if index > len(self.STEPS):
            raise AssertionError('unexpected extra model round')
        revision = None
        results = []
        for previous in range(index):
            results.append(self.result(body, f'fixture_{previous}'))
        if results:
            revision = results[-1]['revision']
        with self.lock:
            self.requests.append({'round': index, 'tool_results': results})
        offered = {t['function']['name'] for t in body.get('tools', [])}
        if not set(self.STEPS).issubset(offered):
            raise AssertionError('editor did not advertise the required shared tools')
        if index == len(self.STEPS):
            with self.lock:
                self.completed = True
            return self.response({'content': '已创建 AI球，移动并验证撤销重做。'})
        name = self.STEPS[index]
        args = {}
        if name == 'set_playback':
            args = {'paused': True}
        elif name == 'spawn_primitive':
            args = {'name': 'AI球', 'shape': {'type': 'sphere', 'radius': 0.5},
                    'position': [0, 2, 0]}
        elif name == 'set_transform':
            args = {'name': 'AI球', 'position': [2, 3, 4]}
        if name not in ('inspect_scene', 'set_playback'):
            args['expected_revision'] = revision
        return self.call(index, name, args)


def make_upstream(script):
    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self, *_args):
            pass

        def send_json(self, value, status=200):
            data = json.dumps(value, ensure_ascii=False).encode()
            self.send_response(status)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(data)))
            self.end_headers()
            try:
                self.wfile.write(data)
            except (BrokenPipeError, ConnectionResetError):
                pass

        def do_GET(self):
            if self.path == '/fixture/status':
                self.send_json(script.report())
            else:
                self.send_json({'error': 'unknown route'}, 404)

        def do_POST(self):
            if self.path == '/fixture/release':
                script.release.set()
                self.send_json({'released': True})
                return
            if self.path != '/v1/chat/completions':
                self.send_json({'error': 'unknown route'}, 404)
                return
            try:
                body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
                response = script.reply(body)
                self.send_json(response)
                if response['choices'][0]['message'].get('tool_calls', [{}])[0].get('id') == 'fixture_late':
                    script.late_sent.set()
            except (AssertionError, ValueError, KeyError, TypeError) as error:
                with script.lock:
                    script.errors.append(str(error))
                self.send_json({'error': str(error)}, 500)
    return http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)


def serve(web_root):
    script = ModelScript()
    upstream = make_upstream(script)
    proxy = create_server(port=0, web_root=web_root, config=Config(
        base_url=f'http://127.0.0.1:{upstream.server_port}/v1',
        model='loopback-fixture', api_key='fixture-only-no-provider-key', timeout=25))
    threads = []
    for server in (upstream, proxy):
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        threads.append(thread)
    print(json.dumps({'url': f'http://127.0.0.1:{proxy.server_port}/?ui_debug=1',
                      'control': f'http://127.0.0.1:{upstream.server_port}'}), flush=True)
    try:
        for _line in sys.stdin:
            pass
    finally:
        script.release.set()
        for server in (proxy, upstream):
            server.shutdown()
            server.server_close()
        for thread in threads:
            thread.join(timeout=2)


class FixtureTests(unittest.TestCase):
    def test_script_uses_actual_tool_result_revision_and_preserves_null(self):
        script = ModelScript()
        body = {'messages': [{'role': 'user', 'content': 'fixture-main'}],
                'tools': [{'function': {'name': name}} for name in set(script.STEPS)]}
        for index, expected in enumerate(script.STEPS):
            message = script.reply(body)['choices'][0]['message']
            self.assertIsNone(message['content'])
            call = message['tool_calls'][0]
            self.assertEqual(call['function']['name'], expected)
            args = json.loads(call['function']['arguments'])
            if expected not in ('inspect_scene', 'set_playback'):
                self.assertEqual(args['expected_revision'], f'actual-{index - 1}')
            body['messages'].extend([message, {'role': 'tool', 'tool_call_id': call['id'],
                'content': json.dumps({'ok': True, 'revision': f'actual-{index}'})}])
        self.assertIn('AI球', script.reply(body)['choices'][0]['message']['content'])
        self.assertTrue(script.report()['completed'])

    def test_failed_or_duplicate_tool_results_stop_script(self):
        body = {'messages': [{'role': 'tool', 'tool_call_id': 'fixture_0',
                             'content': '{"ok":false,"revision":"r"}'}], 'tools': []}
        with self.assertRaises(AssertionError):
            ModelScript().reply(body)
        body['messages'][0]['content'] = '{"ok":true,"revision":"r"}'
        body['messages'].append(body['messages'][0].copy())
        with self.assertRaises(AssertionError):
            ModelScript().reply(body)

    def test_cancel_reply_waits_for_explicit_release(self):
        script = ModelScript()
        replies = []
        thread = threading.Thread(target=lambda: replies.append(script.reply({
            'messages': [{'role': 'user', 'content': 'fixture-cancel:{"revision":"actual-cancel"}'}]})))
        thread.start()
        self.assertTrue(script.delayed.wait(1))
        self.assertEqual(replies, [])
        script.release.set()
        thread.join(timeout=1)
        self.assertFalse(thread.is_alive())
        late = replies[0]['choices'][0]['message']['tool_calls'][0]
        self.assertEqual(late['id'], 'fixture_late')
        self.assertEqual(json.loads(late['function']['arguments'])['expected_revision'], 'actual-cancel')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--serve', action='store_true')
    parser.add_argument('--web-root', type=Path, default=Path(__file__).resolve().parent.parent / 'web')
    args, remaining = parser.parse_known_args()
    if args.serve:
        serve(args.web_root)
    else:
        unittest.main(argv=[sys.argv[0], *remaining])
