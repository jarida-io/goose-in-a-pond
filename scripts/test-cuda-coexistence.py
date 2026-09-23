#!/usr/bin/env python3
"""Exercise Whisper and llama.cpp CUDA in one scratch Pond process on Jetson.

Requires a release binary with both CUDA features and existing public model files.
No production database or model file is modified. Logs remain in the chosen output
folder on success or failure; scratch state is removed after its child exits.
"""
import argparse
import importlib.util
import json
import re
import signal
from pathlib import Path
import shutil
import subprocess
import tempfile
import urllib.request
import wave


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--llm', type=Path, required=True)
    parser.add_argument('--whisper', type=Path, required=True)
    parser.add_argument('--voice-assets', type=Path, required=True)
    parser.add_argument('--embedding-assets', type=Path,
                        help='Reuse public embedding model assets without copying production databases')
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--diagnose-shutdown', action='store_true',
                        help='Capture a GDB thread trace if the scratch process fails to exit')
    args = parser.parse_args()
    for path in (args.binary, args.llm, args.whisper):
        assert path.is_file(), f'Missing input: {path}'
    args.output.mkdir(parents=True, exist_ok=True)
    spec = importlib.util.spec_from_file_location('https_checks', Path(__file__).with_name('test-pinned-https.py'))
    checks = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(checks)
    checks.BINARY = args.binary.resolve()
    with tempfile.TemporaryDirectory(prefix='pond-cuda-coexistence-') as directory:
        data = Path(directory)
        models = data / 'models'
        (models / 'gguf').mkdir(parents=True)
        (models / 'gguf/cuda-smoke.gguf').symlink_to(args.llm.resolve())
        (models / 'ggml-base.en.bin').symlink_to(args.whisper.resolve())
        shutil.copytree(args.voice_assets, models / 'kokoro', symlinks=False)
        if args.embedding_assets:
            shutil.copytree(args.embedding_assets, models / 'embedding', symlinks=False)
        token = None
        for phase in ('configure', 'cuda'):
            with (args.output / f'{phase}.log').open('wb') as log:
                process, port, _ = checks.start(data, log)
                try:
                    holders = subprocess.check_output(['lsof', '-t', '-nP', f'-iTCP:{port}', '-sTCP:LISTEN'], text=True).split()
                    assert str(process.pid) in holders, 'Refusing to test a foreign server'
                    base = f'http://127.0.0.1:{port}'

                    def request(path, body=None, method=None, timeout=180):
                        headers = {'Content-Type': 'application/json'}
                        if token:
                            headers['Authorization'] = 'Bearer ' + token
                        req = urllib.request.Request(base + path, data=None if body is None else json.dumps(body).encode(), headers=headers, method=method)
                        with urllib.request.urlopen(req, timeout=timeout) as response:
                            assert response.status == 200, response.status
                            return json.load(response)

                    if phase == 'configure':
                        # Existing LAN pairing grants only this disposable test a token.
                        code = request('/api/v1/handshake/pairing-code', {}, 'POST')['code']
                        paired = request('/api/v1/handshake', {'client_id': 'cuda-smoke', 'client_type': 'gotg', 'client_version': 'test', 'pairing_code': code}, 'POST')
                        assert paired.get('accepted') and paired.get('session_token'), 'Scratch pairing failed'
                        token = paired['session_token']
                        request('/api/v1/settings', {
                            'user_name': 'CUDA smoke test', 'assistant_name': 'Goose',
                            'timezone': 'UTC', 'chat_provider': 'local', 'chat_model': 'cuda-smoke.gguf',
                            'active_whisper_model': 'ggml-base.en.bin', 'embedding_provider': 'none',
                            'mic_enabled': False, 'llm_max_tokens': 32, 'thinking_mode': 'off',
                        }, 'PUT')
                        request('/api/v1/onboard/complete', {}, 'POST')
                        request('/api/v1/models/scan', {}, 'POST')
                    else:
                        wav = data / 'silence.wav'
                        with wave.open(str(wav), 'wb') as audio:
                            audio.setnchannels(1); audio.setsampwidth(2); audio.setframerate(16000)
                            audio.writeframes(bytes(16000 * 2 * 2))
                        result = subprocess.check_output(['curl', '--silent', '--show-error', '--max-time', '120',
                            '--write-out', '\n%{http_code}', '-H', 'Authorization: Bearer ' + token,
                            '-F', f'audio=@{wav};type=audio/wav', base + '/api/v1/transcribe'], text=True)
                        payload, status = result.rsplit('\n', 1)
                        assert status == '200' and isinstance(json.loads(payload).get('text'), str), result
                        print('PASS in-process Whisper transcription', flush=True)
                        response = request('/api/v1/chat', {'message': 'Reply with the single word hello.', 'voice_mode': True}, 'POST', timeout=240)
                        assert isinstance(response.get('response'), str) and response['response'].strip(), response
                        print('PASS local llama.cpp generation after Whisper in the same process', flush=True)
                        assert process.poll() is None, 'Pond crashed after native execution'
                        request('/api/v1/health')
                finally:
                    process.send_signal(signal.SIGTERM)
                    try:
                        process.wait(timeout=15)
                    except subprocess.TimeoutExpired:
                        try:
                            if args.diagnose_shutdown:
                                with (args.output / f'{phase}-shutdown.txt').open('wb') as diagnostic:
                                    subprocess.run(['gdb', '--batch', '-p', str(process.pid),
                                                    '-ex', 'thread apply all bt', '-ex', 'detach'],
                                                   stdout=diagnostic, stderr=diagnostic, timeout=30)
                        finally:
                            process.kill()
                            process.wait()
                        raise AssertionError('Scratch Pond did not shut down within 15 seconds')
                    assert process.returncode == 0, f'Pond exited with {process.returncode}'
        text = (args.output / 'cuda.log').read_text(errors='replace')
        assert re.search(r'offloaded [1-9][0-9]*/[0-9]+ layers to GPU', text), 'Missing evidence of LLM GPU offload'
        assert 'whisper_backend_init_gpu: using CUDA' in text, 'Missing Whisper GPU initialization evidence'
        (args.output / 'result.json').write_text(json.dumps({'passed': True, 'same_process': True, 'binary': str(args.binary.resolve())}) + '\n')
        print('PASS clean shutdown and combined CUDA execution; inspect retained logs for backend details', flush=True)


if __name__ == '__main__':
    main()
