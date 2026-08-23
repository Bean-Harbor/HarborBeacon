import os
import re
import stat
import subprocess
import tempfile
import unittest
from pathlib import Path


REPOSITORY_ROOT = Path(__file__).parents[1]
POSTINST_PATH = REPOSITORY_ROOT / "debian" / "postinst"
RECONCILER_PATH = (
    REPOSITORY_ROOT / "debian" / "reconcile-legacy-semantic-router"
)
RELEASE_WORKFLOW_PATH = (
    REPOSITORY_ROOT / ".github" / "workflows" / "release.yml"
)
UNIT_PATH = REPOSITORY_ROOT / "debian" / "harboros-beacon.service"


def read_text(path):
    return path.read_text(encoding="utf-8")


class Amd64SemanticRouterTopologyTests(unittest.TestCase):
    def test_postinst_selects_embedded_topology_and_reconciles_legacy_unit(self):
        postinst = read_text(POSTINST_PATH)

        self.assertRegex(
            postinst,
            re.compile(
                r'HARBOR_SEMANTIC_ROUTER_TOPOLOGY"\s+"embedded"'
            ),
            "the formal AMD64 environment must select the embedded router topology",
        )
        self.assertIn(
            "reconcile-legacy-semantic-router",
            postinst,
            "postinst must retire an installed legacy standalone router",
        )
        self.assertNotIn(
            "ensure_semantic_router_runtime_env",
            postinst,
            "fresh installs must not create a standalone router environment",
        )
        self.assertNotIn(
            "127.0.0.1:4176",
            postinst,
            "the formal package must not publish the legacy standalone endpoint",
        )
        self.assertIn(
            'upsert_env_value "$env_file" "HARBOR_MODEL_API_BASE_URL" '
            '"http://127.0.0.1:4174/api/inference/v1"',
            postinst,
            "upgrades must replace a stale or attacker-controlled model facade URL",
        )

        gate_preflight = postinst.index("validate-harborbeacon-service-auth")
        model_token = postinst.index("ensure-harborbeacon-model-token")
        embedded_topology = postinst.index("\nensure_harborbeacon_runtime_env\n")
        self.assertLess(gate_preflight, model_token)
        self.assertLess(model_token, embedded_topology)

        forbidden_activation = re.compile(
            r"(?m)^\s*systemctl\s+(?:--\S+\s+)*"
            r"(?:enable|start|restart|reenable|preset|try-restart|reload-or-restart)"
            r"\b[^\n]*\bsemantic-router\.service\b"
        )
        self.assertNotRegex(
            postinst,
            forbidden_activation,
            "postinst may retire, but must never activate, the standalone router",
        )

        reconciler_call = postinst.index("reconcile-legacy-semantic-router")
        beacon_restart = postinst.index("systemctl restart harboros-beacon.service")
        self.assertLess(
            reconciler_call,
            beacon_restart,
            "legacy retirement must finish before Beacon is restarted",
        )

    def test_legacy_reconciler_is_fail_closed_and_preserves_secret_mode(self):
        reconciler = read_text(RECONCILER_PATH)

        self.assertIn("set -euo pipefail", reconciler)
        self.assertIn("semantic-router.service", reconciler)
        self.assertIn("/etc/default/semantic-router", reconciler)
        self.assertLess(
            reconciler.index('[ ! -L "$legacy_env" ]'),
            reconciler.index('[ -e "$legacy_env" ]'),
            "symlinks, including dangling ones, must be rejected before existence checks",
        )
        self.assertIn("expected_uid=0", reconciler)
        self.assertIn("expected_gid=0", reconciler)
        self.assertIn("stat -c '%u'", reconciler)
        self.assertIn("stat -c '%g'", reconciler)
        self.assertRegex(
            reconciler,
            re.compile(r"(?:chmod\s+0?600|install\b[^\n]*\s-m\s+0?600)"),
            "a retained legacy environment must remain root-only",
        )
        self.assertRegex(
            reconciler,
            re.compile(
                r'(?:systemctl|"\$systemctl_bin")\s+'
                r'(?:--\S+\s+)*stop\b[^\n]*(?:semantic-router|"\$unit")'
            ),
            "the reconciler must stop an active legacy router",
        )
        self.assertRegex(
            reconciler,
            re.compile(
                r'(?:systemctl|"\$systemctl_bin")\s+'
                r'(?:--\S+\s+)*disable\b[^\n]*(?:semantic-router|"\$unit")'
            ),
            "the reconciler must disable an enabled legacy router",
        )

        forbidden_activation = re.compile(
            r'(?m)^\s*(?:systemctl|"\$systemctl_bin")\s+(?:--\S+\s+)*'
            r"(?:enable|start|restart|reenable|preset|try-restart|reload-or-restart)"
            r'\b[^\n]*(?:\bsemantic-router\.service\b|"\$unit")'
        )
        self.assertNotRegex(reconciler, forbidden_activation)

    @unittest.skipUnless(os.name == "posix", "maintainer scripts require POSIX")
    def test_legacy_reconciler_is_idempotent_redacted_and_secures_mode(self):
        secret = "legacy_router_secret_0123456789abcdef"
        with tempfile.TemporaryDirectory() as temp_directory:
            test_root = Path(temp_directory) / "root"
            env_path = test_root / "etc" / "default" / "semantic-router"
            wants_path = (
                test_root
                / "etc"
                / "systemd"
                / "system"
                / "multi-user.target.wants"
                / "semantic-router.service"
            )
            fake_state = Path(temp_directory) / "systemctl-state"
            fake_systemctl = Path(temp_directory) / "systemctl"
            fake_log = Path(temp_directory) / "systemctl.log"

            env_path.parent.mkdir(parents=True)
            env_path.write_text(
                f"HARBOR_MODEL_API_TOKEN={secret}\n",
                encoding="utf-8",
            )
            env_path.chmod(0o644)
            wants_path.parent.mkdir(parents=True)
            wants_path.symlink_to("../semantic-router.service")
            fake_state.mkdir()
            (fake_state / "active").touch()
            (fake_state / "enabled").touch()
            fake_systemctl.write_text(
                """#!/bin/sh
set -eu
printf '%s\\n' "$*" >> "$FAKE_SYSTEMCTL_LOG"
case "$1" in
  is-active)  test -f "$FAKE_SYSTEMCTL_STATE/active" ;;
  is-enabled) test -f "$FAKE_SYSTEMCTL_STATE/enabled" ;;
  stop)       rm -f "$FAKE_SYSTEMCTL_STATE/active" ;;
  disable)    rm -f "$FAKE_SYSTEMCTL_STATE/enabled" ;;
  daemon-reload) : ;;
  *) exit 64 ;;
esac
""",
                encoding="utf-8",
            )
            fake_systemctl.chmod(0o755)
            command_env = os.environ.copy()
            command_env.update(
                {
                    "FAKE_SYSTEMCTL_LOG": str(fake_log),
                    "FAKE_SYSTEMCTL_STATE": str(fake_state),
                }
            )
            command = [
                "bash",
                str(RECONCILER_PATH),
                "--test-root",
                str(test_root),
                "--systemctl",
                str(fake_systemctl),
            ]

            results = [
                subprocess.run(
                    command,
                    env=command_env,
                    text=True,
                    capture_output=True,
                    check=False,
                )
                for _ in range(2)
            ]

            for result in results:
                self.assertEqual(0, result.returncode, result.stderr)
                self.assertNotIn(secret, result.stdout + result.stderr)
            self.assertEqual(
                f"HARBOR_MODEL_API_TOKEN={secret}\n",
                env_path.read_text(encoding="utf-8"),
            )
            self.assertEqual(0o600, stat.S_IMODE(env_path.stat().st_mode))
            self.assertFalse(wants_path.exists())
            self.assertFalse(wants_path.is_symlink())
            self.assertFalse((fake_state / "active").exists())
            self.assertFalse((fake_state / "enabled").exists())
            self.assertNotIn(secret, fake_log.read_text(encoding="utf-8"))

    @unittest.skipUnless(os.name == "posix", "maintainer scripts require POSIX")
    def test_legacy_reconciler_rejects_env_symlink_without_touching_target(self):
        secret = "protected_router_secret_0123456789abcdef"
        with tempfile.TemporaryDirectory() as temp_directory:
            test_root = Path(temp_directory) / "root"
            env_path = test_root / "etc" / "default" / "semantic-router"
            protected_path = Path(temp_directory) / "protected.env"
            fake_systemctl = Path(temp_directory) / "systemctl"
            fake_log = Path(temp_directory) / "systemctl.log"

            env_path.parent.mkdir(parents=True)
            protected_path.write_text(secret, encoding="utf-8")
            env_path.symlink_to(protected_path)
            fake_systemctl.write_text(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$FAKE_SYSTEMCTL_LOG\"\n",
                encoding="utf-8",
            )
            fake_systemctl.chmod(0o755)
            command_env = os.environ.copy()
            command_env["FAKE_SYSTEMCTL_LOG"] = str(fake_log)

            result = subprocess.run(
                [
                    "bash",
                    str(RECONCILER_PATH),
                    "--test-root",
                    str(test_root),
                    "--systemctl",
                    str(fake_systemctl),
                ],
                env=command_env,
                text=True,
                capture_output=True,
                check=False,
            )

            self.assertNotEqual(0, result.returncode)
            self.assertEqual(secret, protected_path.read_text(encoding="utf-8"))
            self.assertNotIn(secret, result.stdout + result.stderr)
            self.assertFalse(fake_log.exists())

    @unittest.skipUnless(os.name == "posix", "maintainer scripts require POSIX")
    def test_legacy_reconciler_rejects_dangling_env_symlink(self):
        with tempfile.TemporaryDirectory() as temp_directory:
            test_root = Path(temp_directory) / "root"
            env_path = test_root / "etc" / "default" / "semantic-router"
            missing_target = Path(temp_directory) / "missing.env"
            fake_systemctl = Path(temp_directory) / "systemctl"
            fake_log = Path(temp_directory) / "systemctl.log"

            env_path.parent.mkdir(parents=True)
            env_path.symlink_to(missing_target)
            fake_systemctl.write_text(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$FAKE_SYSTEMCTL_LOG\"\n",
                encoding="utf-8",
            )
            fake_systemctl.chmod(0o755)
            command_env = os.environ.copy()
            command_env["FAKE_SYSTEMCTL_LOG"] = str(fake_log)

            result = subprocess.run(
                [
                    "bash",
                    str(RECONCILER_PATH),
                    "--test-root",
                    str(test_root),
                    "--systemctl",
                    str(fake_systemctl),
                ],
                env=command_env,
                text=True,
                capture_output=True,
                check=False,
            )

            self.assertNotEqual(0, result.returncode)
            self.assertIn("must not be a symlink", result.stderr)
            self.assertTrue(env_path.is_symlink())
            self.assertFalse(missing_target.exists())
            self.assertFalse(fake_log.exists())

    @unittest.skipUnless(os.name == "posix", "maintainer scripts require POSIX")
    def test_legacy_reconciler_rejects_unsafe_env_mode_without_mutation(self):
        secret = "unsafe_mode_secret_0123456789abcdef"
        with tempfile.TemporaryDirectory() as temp_directory:
            test_root = Path(temp_directory) / "root"
            env_path = test_root / "etc" / "default" / "semantic-router"
            fake_systemctl = Path(temp_directory) / "systemctl"
            fake_log = Path(temp_directory) / "systemctl.log"

            env_path.parent.mkdir(parents=True)
            env_path.write_text(secret, encoding="utf-8")
            env_path.chmod(0o666)
            fake_systemctl.write_text(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$FAKE_SYSTEMCTL_LOG\"\n",
                encoding="utf-8",
            )
            fake_systemctl.chmod(0o755)
            command_env = os.environ.copy()
            command_env["FAKE_SYSTEMCTL_LOG"] = str(fake_log)

            result = subprocess.run(
                [
                    "bash",
                    str(RECONCILER_PATH),
                    "--test-root",
                    str(test_root),
                    "--systemctl",
                    str(fake_systemctl),
                ],
                env=command_env,
                text=True,
                capture_output=True,
                check=False,
            )

            self.assertNotEqual(0, result.returncode)
            self.assertIn("unsafe mode", result.stderr)
            self.assertEqual(secret, env_path.read_text(encoding="utf-8"))
            self.assertEqual(0o666, stat.S_IMODE(env_path.stat().st_mode))
            self.assertNotIn(secret, result.stdout + result.stderr)
            self.assertFalse(fake_log.exists())

    def test_release_package_installs_the_legacy_reconciler(self):
        workflow = read_text(RELEASE_WORKFLOW_PATH)

        self.assertIn("reconcile-legacy-semantic-router", workflow)
        self.assertRegex(
            workflow,
            re.compile(
                r"chmod\s+0?755\s+[^\n]*reconcile-legacy-semantic-router"
            ),
            "the helper invoked by postinst must be executable in the archive",
        )
        self.assertIn(
            'find "${PKG_DIR}" -type d -exec chmod a-s,u=rwx,go=rx {} +',
            workflow,
            "the package tree must not inherit setgid or unsafe directory modes",
        )
        self.assertIn("dpkg-deb --root-owner-group --build", workflow)
        for helper in [
            "validate-harborbeacon-service-auth",
            "ensure-harborbeacon-model-token",
            "reconcile-legacy-semantic-router",
        ]:
            self.assertIn(helper, workflow)
            self.assertIn(
                f"./usr/lib/harboros-beacon/{helper}",
                workflow,
                f"release packaging must assert {helper} is in the deb",
            )
        self.assertIn(
            "harbor-model-api|semantic-router\\.service|etc/default/semantic-router",
            workflow,
        )

    def test_systemd_unit_combines_auth_recovery_model_env_and_exact_bind(self):
        unit = read_text(UNIT_PATH)

        self.assertIn("Requires=harboros-service-auth-recovery.service", unit)
        self.assertIn("LoadCredential=gate-to-beacon-accept-current:", unit)
        self.assertIn("LoadCredential=gate-to-beacon-accept-previous:", unit)
        self.assertIn("LoadCredential=beacon-to-gate-send:", unit)
        self.assertIn("EnvironmentFile=-/etc/default/harboros-beacon", unit)
        self.assertIn("--bind 127.0.0.1:4174", unit)
        self.assertNotIn("127.0.0.1:4176", unit)


if __name__ == "__main__":
    unittest.main()
