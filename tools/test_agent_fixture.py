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
import urllib.error
import urllib.request

from agent_server import create_server


class ModelScript:
    STEPS = ('set_playback', 'inspect_scene', 'spawn_primitive', 'set_transform',
             'inspect_scene', 'undo', 'inspect_scene', 'redo', 'inspect_scene')

    def __init__(self):
        self.lock = threading.Lock()
        self.release = threading.Event()
        self.delayed = threading.Event()
        self.requests = []
        self.models_requests = []
        self.errors = []
        self.completed = False
        self.cancel_requests = 0
        self.followup_requests = []
        self.long_requests = []
        self.long_completed = False
        self.seen_compaction = False
        self.late_sent = threading.Event()
        self.orbit_requests = []
        self.orbit_stop_requests = []

    def report(self):
        with self.lock:
            return {'requests': self.requests.copy(), 'errors': self.errors.copy(),
                    'completed': self.completed, 'delayed': self.delayed.is_set(),
                    'cancel_requests': self.cancel_requests, 'late_sent': self.late_sent.is_set(),
                    'models_requests': self.models_requests.copy(),
                    'followup_requests': self.followup_requests.copy(),
                    'long_requests': self.long_requests.copy(),
                    'long_completed': self.long_completed,
                    'seen_compaction': self.seen_compaction,
                    'orbit_requests': self.orbit_requests.copy(),
                    'orbit_stop_requests': self.orbit_stop_requests.copy()}

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
        message = {'tool_calls': [{
            'id': f'fixture_{index}', 'type': 'function',
            'function': {'name': name, 'arguments': json.dumps(args, ensure_ascii=False)}}]}
        # Both forms are valid OpenAI tool messages. Exercise omission through
        # the real HTTP/WASM path, rather than only a parser unit test.
        if index not in (0, 1):
            message['content'] = None
        return cls.response(message)

    def reply(self, body):
        prompts = [m.get('content', '') for m in body['messages'] if m.get('role') == 'user']
        latest_prompt = str(prompts[-1]) if prompts else ''
        if latest_prompt.startswith('fixture-orbit'):
            stopping = latest_prompt.startswith('fixture-orbit-stop')
            prefix = 'orbit_stop_' if stopping else 'orbit_'
            calls = [call for message in body['messages']
                     if message.get('role') == 'assistant'
                     for call in message.get('tool_calls', [])
                     if str(call.get('id', '')).startswith(prefix)
                     and str(call.get('id', '')).removeprefix(prefix).isdigit()]
            index = int(calls[-1]['id'].removeprefix(prefix)) + 1 if calls else 0
            steps = [('set_playback', {'paused': True})] if stopping else [
                ('set_playback', {'paused': True}),
                ('inspect_scene', {}),
                ('spawn_primitive', {'name': '太阳',
                    'shape': {'type': 'sphere', 'radius': 0.8}, 'position': [0, 5, 0]}),
                ('set_body_mode', {'name': '太阳', 'mode': 'static'}),
                ('spawn_primitive', {'name': '地球',
                    'shape': {'type': 'sphere', 'radius': 0.2}, 'position': [3, 5, 0]}),
                ('set_orbit', {'name': '地球', 'center': [0, 5, 0],
                    'radius': 3, 'period_secs': 2, 'plane': 'xz', 'phase_deg': 0}),
                ('spawn_primitive', {'name': '月球',
                    'shape': {'type': 'sphere', 'radius': 0.1}, 'position': [4, 5, 0]}),
                ('set_orbit', {'name': '月球', 'center_name': '地球',
                    'radius': 1, 'period_secs': 0.5, 'plane': 'xz', 'phase_deg': 0}),
                ('set_object_label', {'name': '太阳', 'visible': True}),
                ('set_object_label', {'name': '地球', 'visible': True}),
                ('set_object_label', {'name': '月球', 'visible': True}),
                ('set_playback', {'paused': False})]
            if not 0 <= index <= len(steps):
                raise AssertionError('unexpected orbit fixture round')
            receipt = self.result(body, calls[-1]['id']) if calls else None
            offered = {tool['function']['name'] for tool in body.get('tools', [])}
            if not {name for name, _args in steps}.issubset(offered):
                raise AssertionError('editor did not advertise orbit/body-mode/label tools')
            with self.lock:
                target = self.orbit_stop_requests if stopping else self.orbit_requests
                target.append({'round': index, 'last_revision': receipt['revision'] if receipt else None})
            if index == len(steps):
                return self.response({'content': 'fixture-orbit-stopped' if stopping else 'fixture-orbit-done'})
            name, args = steps[index]
            if name not in ('inspect_scene', 'set_playback'):
                args['expected_revision'] = receipt['revision']
            return self.response({'tool_calls': [{'id': f'{prefix}{index}', 'type': 'function',
                'function': {'name': name, 'arguments': json.dumps(args)}}]})
        if latest_prompt.startswith('fixture-long'):
            # Infer progress from the latest surviving call, not total transcript
            # length: older calls are intentionally removed from request context.
            calls = [call for message in body['messages']
                     if message.get('role') == 'assistant'
                     for call in message.get('tool_calls', [])
                     if str(call.get('id', '')).startswith('long_')]
            index = int(calls[-1]['id'].removeprefix('long_')) + 1 if calls else 0
            if not 0 <= index <= 40:
                raise AssertionError('unexpected long fixture round')
            if calls:
                self.result(body, calls[-1]['id'])
            if 'inspect_scene' not in {tool['function']['name'] for tool in body.get('tools', [])}:
                raise AssertionError('long fixture needs inspect_scene tool')
            systems = sum(message.get('role') == 'system' for message in body['messages'])
            message_bytes = len(json.dumps(body['messages'], ensure_ascii=False,
                                          separators=(',', ':')).encode())
            request_bytes = len(json.dumps(body, ensure_ascii=False,
                                          separators=(',', ':')).encode())
            with self.lock:
                self.long_requests.append({'round': index, 'message_count': len(body['messages']),
                    'message_bytes': message_bytes, 'request_bytes': request_bytes,
                    'system_count': systems})
                self.seen_compaction |= systems > 1
                if index == 40:
                    self.long_completed = True
            if index == 40:
                return self.response({'content': 'fixture-long-done: 40 次检查已完成，长上下文和工具回执保持一致。'})
            return self.response({'content': f'progress {index}: ' + 'x' * 2000,
                'tool_calls': [{'id': f'long_{index}', 'type': 'function',
                    'function': {'name': 'inspect_scene', 'arguments': '{}'}}]})
        if 'fixture-followup' in latest_prompt:
            if not any('fixture-main' in str(p) for p in prompts[:-1]):
                raise AssertionError('followup lost the original user prompt')
            results = [self.result(body, f'fixture_{index}') for index in range(len(self.STEPS))]
            final_scene = results[-1].get('result', {})
            objects = final_scene.get('objects', [])
            if not any(o.get('name') == 'AI球' and o.get('position') == [2, 3, 4] for o in objects):
                raise AssertionError('followup lost actual model-created AI球 tool results')
            if not any(m.get('role') == 'assistant' and '已创建 AI球' in str(m.get('content', ''))
                       for m in body['messages']):
                raise AssertionError('followup lost the assistant completion')
            with self.lock:
                self.followup_requests.append({'user_prompts': prompts.copy(),
                    'tool_results': len(results), 'message_count': len(body['messages'])})
            return self.response({'content': 'fixture-followup-ok：AI球位于 [2,3,4]，此前工具结果和对话均已保留。'})
        if 'fixture-cancel' in latest_prompt:
            with self.lock:
                self.cancel_requests += 1
            self.delayed.set()
            if not self.release.wait(20):
                raise AssertionError('cancel response was never released by probe')
            cancel_prompt = latest_prompt
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
            elif self.path == '/v1/models':
                authenticated = self.headers.get('Authorization') == 'Bearer fixture-client-only-key'
                with script.lock:
                    script.models_requests.append({'path': self.path, 'authenticated': authenticated})
                if not authenticated:
                    self.send_json({'error': 'client provider credential required'}, 401)
                    return
                self.send_json({'object': 'list', 'data': [
                    {'id': 'model-alpha', 'object': 'model', 'owned_by': 'fixture'},
                    {'id': 'model-beta', 'object': 'model', 'owned_by': 'fixture'},
                ]})
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
                if self.headers.get('Authorization') != 'Bearer fixture-client-only-key':
                    raise AssertionError('model request must use the client-saved provider credential')
                if body.get('model') != 'loopback-fixture' or body.get('max_tokens') != 2048:
                    raise AssertionError('model options must come from the client configuration')
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
    proxy = create_server(port=0, web_root=web_root)
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
    def test_model_catalog_requires_client_auth_and_reports_requests(self):
        script = ModelScript()
        server = make_upstream(script)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        url = f'http://127.0.0.1:{server.server_port}/v1/models'
        try:
            with self.assertRaises(urllib.error.HTTPError) as rejected:
                urllib.request.urlopen(url, timeout=2)
            self.assertEqual(rejected.exception.code, 401)
            request = urllib.request.Request(url, headers={
                'Authorization': 'Bearer fixture-client-only-key'})
            with urllib.request.urlopen(request, timeout=2) as response:
                catalog = json.load(response)
            self.assertEqual([model['id'] for model in catalog['data']],
                             ['model-alpha', 'model-beta'])
            self.assertEqual(script.report()['models_requests'], [
                {'path': '/v1/models', 'authenticated': False},
                {'path': '/v1/models', 'authenticated': True}])
        finally:
            server.shutdown()
            server.server_close()
            thread.join(timeout=2)

    def test_script_uses_actual_tool_result_revision_and_accepts_omitted_content(self):
        script = ModelScript()
        body = {'messages': [{'role': 'user', 'content': 'fixture-main'}],
                'tools': [{'function': {'name': name}} for name in set(script.STEPS)]}
        for index, expected in enumerate(script.STEPS):
            message = script.reply(body)['choices'][0]['message']
            if index < 2:
                self.assertNotIn('content', message)
            else:
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

    def test_followup_requires_original_context_and_latest_user_selects_route(self):
        script = ModelScript()
        body = {'messages': [{'role': 'user', 'content': 'fixture-main'}],
                'tools': [{'function': {'name': name}} for name in set(script.STEPS)]}
        for index in range(len(script.STEPS)):
            message = script.reply(body)['choices'][0]['message']
            receipt = {'ok': True, 'revision': f'actual-{index}', 'result': {
                'objects': [{'name': 'AI球', 'position': [2, 3, 4]}]}}
            body['messages'].extend([message, {'role': 'tool', 'tool_call_id': f'fixture_{index}',
                'content': json.dumps(receipt)}])
        body['messages'].append(script.reply(body)['choices'][0]['message'])
        # An earlier cancel marker must not hijack a later followup request.
        body['messages'].extend([{'role': 'user', 'content': 'fixture-cancel:old'},
                                 {'role': 'user', 'content': 'fixture-followup'}])
        self.assertIn('fixture-followup-ok', script.reply(body)['choices'][0]['message']['content'])
        self.assertEqual(script.report()['cancel_requests'], 0)
        self.assertEqual(script.report()['followup_requests'][0]['tool_results'], 9)
        body['messages'] = [m for m in body['messages'] if m.get('content') != 'fixture-main']
        with self.assertRaisesRegex(AssertionError, 'original user'):
            script.reply(body)

    def test_orbit_script_uses_receipt_revisions_and_stop_has_no_revision(self):
        script = ModelScript()
        names = ['set_playback', 'inspect_scene', 'spawn_primitive', 'set_body_mode', 'set_orbit', 'set_object_label']
        body = {'messages': [{'role': 'user', 'content': 'fixture-orbit'}],
                'tools': [{'function': {'name': name}} for name in names]}
        expected = ['set_playback', 'inspect_scene', 'spawn_primitive', 'set_body_mode',
                    'spawn_primitive', 'set_orbit', 'spawn_primitive', 'set_orbit',
                    'set_object_label', 'set_object_label', 'set_object_label', 'set_playback']
        for index, name in enumerate(expected):
            message = script.reply(body)['choices'][0]['message']
            call = message['tool_calls'][0]
            self.assertEqual(call['id'], f'orbit_{index}')
            self.assertEqual(call['function']['name'], name)
            args = json.loads(call['function']['arguments'])
            if name in ('set_playback', 'inspect_scene'):
                self.assertNotIn('expected_revision', args)
            else:
                self.assertEqual(args['expected_revision'], f'orbit-rev-{index - 1}')
            if name == 'spawn_primitive':
                self.assertEqual(args['name'], {2: '太阳', 4: '地球', 6: '月球'}[index])
            if name == 'set_object_label':
                self.assertEqual(args['name'], {8: '太阳', 9: '地球', 10: '月球'}[index])
                self.assertIs(args['visible'], True)
            if name == 'set_orbit':
                if index == 5:
                    self.assertEqual(args['name'], '地球')
                    self.assertEqual(args['center'], [0, 5, 0])
                    self.assertNotIn('center_name', args)
                    self.assertEqual(args['radius'], 3)
                    self.assertEqual(args['period_secs'], 2)
                else:
                    self.assertEqual(args['name'], '月球')
                    self.assertEqual(args['center_name'], '地球')
                    self.assertNotIn('center', args)
                    self.assertEqual(args['radius'], 1)
                    self.assertEqual(args['period_secs'], 0.5)
                self.assertEqual(args['plane'], 'xz')
            body['messages'].extend([message, {'role': 'tool', 'tool_call_id': call['id'],
                'content': json.dumps({'ok': True, 'revision': f'orbit-rev-{index}'})}])
        self.assertEqual(script.reply(body)['choices'][0]['message']['content'], 'fixture-orbit-done')
        body['messages'].append({'role': 'user', 'content': 'fixture-orbit-stop'})
        stop = script.reply(body)['choices'][0]['message']
        call = stop['tool_calls'][0]
        self.assertEqual(call['id'], 'orbit_stop_0')
        self.assertEqual(json.loads(call['function']['arguments']), {'paused': True})
        body['messages'].extend([stop, {'role': 'tool', 'tool_call_id': call['id'],
            'content': json.dumps({'ok': True, 'revision': 'orbit-stopped'})}])
        self.assertEqual(script.reply(body)['choices'][0]['message']['content'], 'fixture-orbit-stopped')
        self.assertEqual([item['round'] for item in script.report()['orbit_requests']], list(range(13)))
        self.assertEqual(len(script.report()['orbit_stop_requests']), 2)

    def test_long_script_progress_survives_compressed_context(self):
        script = ModelScript()
        body = {'messages': [{'role': 'system', 'content': 'rules'},
                             {'role': 'user', 'content': 'fixture-long'}],
                'tools': [{'function': {'name': 'inspect_scene'}}]}
        for index in range(40):
            message = script.reply(body)['choices'][0]['message']
            call = message['tool_calls'][0]
            self.assertEqual(call['id'], f'long_{index}')
            self.assertEqual(call['function']['name'], 'inspect_scene')
            self.assertGreaterEqual(len(message['content']), 2000)
            receipt = {'role': 'tool', 'tool_call_id': call['id'],
                       'content': json.dumps({'ok': True, 'revision': f'actual-long-{index}'})}
            # Simulate a bounded sender preserving only the latest call/receipt.
            body['messages'] = [body['messages'][0],
                {'role': 'system', 'content': 'synthetic automatic historical summary'},
                {'role': 'user', 'content': 'fixture-long'}, message, receipt]
        done = script.reply(body)['choices'][0]['message']
        self.assertIn('fixture-long-done', done['content'])
        report = script.report()
        self.assertEqual(len(report['long_requests']), 41)
        self.assertEqual([r['round'] for r in report['long_requests']], list(range(41)))
        self.assertTrue(report['long_completed'])
        self.assertTrue(report['seen_compaction'])
        self.assertLess(report['long_requests'][-1]['request_bytes'], 48000)
        # A previous long marker cannot hijack the next user turn.
        body['messages'].append({'role': 'user', 'content': 'fixture-cancel:{"revision":"actual-last"}'})
        script.release.set()
        self.assertEqual(script.reply(body)['choices'][0]['message']['tool_calls'][0]['id'], 'fixture_late')

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
