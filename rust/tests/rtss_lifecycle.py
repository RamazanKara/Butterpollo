"""Optional RTSS lifecycle regression for interop.py's test-owned host.

Set BUTTERPOLLO_TEST_RTSS_PROFILE to a disposable SDK fixture's Profiles/Global,
and configure that host for RTSS, 59.94 FPS, async mode and retained displays.
Exercises real launch/RTSP/disconnect/resume/expiry ownership with a fake SDK.
It does not establish that a real RTSS installation caps rendered frames.
"""
import configparser
import json
import pathlib
import re
import subprocess
import time
import xml.etree.ElementTree as ET


class Lifecycle:
    def __init__(self, profile, admin, http, web, artifact):
        self.profile = pathlib.Path(profile)
        self.admin, self.http, self.web, self.artifact = admin, http, web, artifact
        self.original = self.values()
        self.applied = {'limit': 2997, 'limitdenominator': 50, 'synclimiter': 0}
        self.checks = []
        assert self.original != self.applied, 'Use distinct initial and test limits'
        assert self.status()['active_provider'] == 'none'

    def values(self):
        profile = configparser.ConfigParser()
        profile.read(self.profile, encoding='utf-8')
        return {key: profile.getint('Framerate', key, fallback=None)
                for key in ['limit', 'limitdenominator', 'synclimiter']}

    def status(self):
        response = self.admin.get(self.web + '/api/frame-limiter/status', timeout=5)
        response.raise_for_status()
        return response.json()

    def launched(self):
        assert self.status()['active_provider'] == 'rtss'
        assert self.values() == self.applied, self.values()

    def restored(self, label, timeout=8):
        deadline = time.monotonic() + timeout
        while True:
            response = self.admin.get(self.http + '/serverinfo', timeout=5)
            response.raise_for_status()
            info = ET.fromstring(response.text)
            ready = (info.findtext('RustHostSessionCount') == '0'
                     and info.findtext('RustHostPendingSessionCount') == '0'
                     and self.status()['active_provider'] == 'none')
            if ready:
                break
            assert time.monotonic() < deadline, label + ': limiter remained active'
            time.sleep(.1)
        assert info.findtext('RustHostApplicationActive') == '1', 'Display must remain retained'
        assert self.values() == self.original, (label, self.values())
        self.checks.append(label)
        print('RTSS LIFECYCLE PASS', label, flush=True)

    def reconnect_and_expire(self, client, https, launch_args, command, env):
        self.restored('disconnect restores the profile while the desktop stays running')

        def resume():
            response = client.get(https + '/resume', params=launch_args, timeout=15)
            response.raise_for_status()
            root = ET.fromstring(response.text)
            assert root.attrib['status_code'] == '200', response.text
            self.launched()
            return root.findtext('sessionUrl0')

        url = resume()
        resumed_env = env.copy()
        resumed_env['BUTTERPOLLO_TEST_TIMING_CSV'] = str(self.artifact / 'resumed-frames.csv')
        resumed = subprocess.run([command[0], url, *command[2:]], env=resumed_env,
                                 capture_output=True, text=True, timeout=45)
        (self.artifact / 'resumed-client.log').write_text(resumed.stdout + resumed.stderr,
                                                       encoding='utf-8')
        assert resumed.returncode == 0, resumed.stdout[-2000:]
        decoded = re.search(r'RESULT frames=(\d+) decoded_frames=(\d+).* failures=(\d+)', resumed.stdout)
        assert decoded and int(decoded[1]) > 0 and decoded[1] == decoded[2] and decoded[3] == '0'
        self.checks.append('resume reapplies the limit and decodes without errors')
        self.restored('resumed stream restores independently of retained display lifetime')
        resume()  # A successful launch whose transport never arrives.
        self.restored('abandoned launch expiry restores the limit', timeout=40)
        result = {'scope': 'real session ownership with simulated RTSS SDK',
                  'passed': True, 'checks': self.checks}
        (self.artifact / 'rtss-lifecycle.json').write_text(json.dumps(result, indent=2), encoding='utf-8')
