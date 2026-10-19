import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


SCRIPTS = Path(__file__).resolve().parent


class ContainerEndpointsTest(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.log = self.root / "commands.jsonl"
        self.domains = self.root / "allowed_domains.txt"
        mock = self.root / "mock"
        mock.write_text(
            "#!/usr/bin/python\n"
            "import json, os, pathlib, sys\n"
            "name = pathlib.Path(sys.argv[0]).name\n"
            "with open(os.environ['COMMAND_LOG'], 'a') as log:\n"
            "    log.write(json.dumps([name, *sys.argv[1:]]) + '\\n')\n"
            "if name == 'dig': print('203.0.113.8')\n"
            "if name == 'ip': print('default via 192.0.2.1 dev eth0')\n"
            "if name == 'curl' and sys.argv[-1] == 'https://example.com': sys.exit(1)\n"
        )
        mock.chmod(0o755)
        for name in ("iptables", "ipset", "dig", "ip", "curl", "docker"):
            (self.root / name).symlink_to(mock)
        self.env = {
            **os.environ,
            "PATH": f"{self.root}:/usr/bin:/bin",
            "COMMAND_LOG": str(self.log),
        }
        self.env.pop("OPENAI_ALLOWED_DOMAINS", None)

    def commands(self):
        return [json.loads(line) for line in self.log.read_text().splitlines()] if self.log.exists() else []

    def firewall(self):
        source = (SCRIPTS / "init_firewall.sh").read_text()
        # Relocate only the fixed configuration path; all commands are mocks.
        source = source.replace("/etc/codex/allowed_domains.txt", str(self.domains))
        return subprocess.run(["/bin/bash", "-c", source], env=self.env, capture_output=True)

    def test_missing_allowlist_fails_before_firewall_or_network_commands(self):
        result = self.firewall()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.commands(), [])

    def test_probes_only_supplied_allowlist(self):
        self.domains.write_text("local.example\ngateway.example\n")
        result = self.firewall()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            [command[-1] for command in self.commands() if command[0] == "curl"],
            ["https://example.com", "https://local.example", "https://gateway.example"],
        )
        self.assertEqual(
            [command[-1] for command in self.commands() if command[0] == "dig"],
            ["local.example", "gateway.example"],
        )

    def test_explicit_openai_domain_remains_valid(self):
        self.domains.write_text("api.openai.com\n")
        self.assertEqual(self.firewall().returncode, 0)
        self.assertIn(["curl", "--connect-timeout", "5", "https://api.openai.com"], self.commands())

    def test_missing_container_allowlist_does_not_touch_docker(self):
        result = subprocess.run(
            ["/bin/bash", str(SCRIPTS / "run_in_container.sh"), "echo hello"],
            env=self.env, capture_output=True,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.commands(), [])


if __name__ == "__main__":
    unittest.main()
