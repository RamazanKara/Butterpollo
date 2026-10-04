"""Contract tests for the Windows CI workflows.

Run with: python .github/scripts/test_ci_workflows.py (needs PyYAML and cmake).
"""
import os
import re
import subprocess
import tempfile
import unittest
from pathlib import Path

import yaml


ROOT = Path(__file__).resolve().parents[2]
WORKFLOWS = ROOT / ".github" / "workflows"
CMAKE_INPUTS = "hashFiles('CMakeLists.txt', 'cmake/**', 'tests/CMakeLists.txt', 'tests/cmake/**')"


def load_workflow(name: str) -> dict:
    with (WORKFLOWS / name).open(encoding="utf-8") as stream:
        # BaseLoader keeps every scalar a string and the "on" key literal.
        return yaml.load(stream, Loader=yaml.BaseLoader)


def build_job() -> dict:
    return load_workflow("ci-windows.yml")["jobs"]["build_windows"]


def step(job: dict, name: str) -> dict:
    return next(s for s in job["steps"] if s.get("name") == name)


class ButterpolloWorkflowTest(unittest.TestCase):
    def test_runs_only_by_hand(self) -> None:
        # The C++ host is a reference since the Rust host (rust-windows.yml).
        triggers = load_workflow("butterpollo-windows.yml")["on"]
        self.assertEqual(set(triggers), {"workflow_dispatch"})

    def test_inputs_match_the_reusable_workflow(self) -> None:
        caller = load_workflow("butterpollo-windows.yml")["jobs"]["windows"]
        self.assertEqual(caller["uses"], "./.github/workflows/ci-windows.yml")
        declared = load_workflow("ci-windows.yml")["on"]["workflow_call"]["inputs"]
        passed = caller["with"]
        self.assertLessEqual(set(passed), set(declared), "caller passes inputs the workflow does not declare")
        required = {name for name, spec in declared.items() if spec.get("required") == "true"}
        self.assertLessEqual(required, set(passed))
        self.assertEqual(passed["build_tests"], "true")
        self.assertEqual(passed["publish_symbols"], "false")

    def test_only_butterpollo_workflows_remain(self) -> None:
        self.assertEqual(
            sorted(path.name for path in WORKFLOWS.glob("*.yml")),
            ["butterpollo-windows.yml", "ci-windows.yml", "rust-windows.yml"],
        )
        for path in WORKFLOWS.glob("*.yml"):
            match = re.search("(?i)signpath", path.read_text(encoding="utf-8"))
            self.assertIsNone(match, f"{path.name} still references SignPath")
        self.assertFalse((ROOT / ".github" / "actions").exists())


class WindowsBuildWorkflowTest(unittest.TestCase):
    def test_single_windows_job_publishes_release_artifacts(self) -> None:
        workflow = load_workflow("ci-windows.yml")
        self.assertEqual(list(workflow["jobs"]), ["build_windows"])
        uploads = {s["with"]["name"]: s["with"]["path"] for s in build_job()["steps"]
                   if s.get("uses", "").startswith("actions/upload-artifact@")}
        self.assertIn("unsigned-msi-package-Windows", uploads)
        self.assertEqual(uploads["build-Windows"], "artifacts/")

        steps = build_job()["steps"]
        names = [s["name"] for s in steps]
        self.assertIn("'release-provenance.json'", step(build_job(), "Record release artifact provenance")["run"])
        self.assertIn("-OutputName $installerName", step(build_job(), "Package Windows installer")["run"])
        self.assertLess(names.index("Package Windows installer"), names.index("Record release artifact provenance"))
        self.assertLess(names.index("Record release artifact provenance"), names.index("Upload Artifacts"))

    def test_checkout_skips_nested_dependency_sources(self) -> None:
        checkout = step(build_job(), "Checkout")
        self.assertEqual(checkout["with"]["fetch-depth"], "1")
        self.assertEqual(checkout["with"]["submodules"], "true")
        self.assertNotIn("build-deps", (ROOT / ".gitmodules").read_text(encoding="utf-8"))
        nested = step(build_job(), "Checkout nested submodules")["run"]
        self.assertIn("third-party/moonlight-common-c submodule update --init --depth 1 enet", nested)

    def test_msys2_packages_are_cached_without_system_upgrade(self) -> None:
        setup = step(build_job(), "Setup Dependencies Windows")["with"]
        self.assertEqual(setup["update"], "false")
        self.assertEqual(setup["cache"], "true")
        packages = setup["install"].split()
        for package in ("boost", "ccache", "cmake", "gcc", "ninja", "python", "python-yaml", "vulkan-headers"):
            self.assertIn(f"mingw-w64-ucrt-x86_64-{package}", packages)

    def test_compiler_cache_wraps_the_build(self) -> None:
        job = build_job()
        names = [s["name"] for s in job["steps"]]
        restore = step(job, "Restore ccache")
        save = step(job, "Save ccache")
        self.assertTrue(restore["uses"].startswith("actions/cache/restore@"))
        self.assertTrue(save["uses"].startswith("actions/cache/save@"))
        self.assertEqual(restore["with"]["path"], save["with"]["path"])
        self.assertEqual(restore["with"]["key"], save["with"]["key"])
        self.assertIn(CMAKE_INPUTS, restore["with"]["key"])
        self.assertIn("${{ github.run_id }}", restore["with"]["key"])
        self.assertIn("ccache-${{ runner.os }}-ucrt64-", restore["with"]["restore-keys"].splitlines())

        configure = step(job, "Configure ccache")["run"]
        for setting in ("CCACHE_DIR=", "CCACHE_BASEDIR=", "CCACHE_COMPRESS=", "CCACHE_MAXSIZE=", "CCACHE_SLOPPINESS="):
            self.assertIn(setting, configure)
        self.assertIn(r"$env:RUNNER_TEMP\ccache", configure)
        self.assertTrue(restore["with"]["path"].startswith("${{ runner.temp }}"))

        build = step(job, "Build Windows")["run"]
        self.assertIn("-DCMAKE_C_COMPILER_LAUNCHER=ccache", build)
        self.assertIn("-DCMAKE_CXX_COMPILER_LAUNCHER=ccache", build)
        self.assertIn("ccache --zero-stats", build)
        self.assertIn("ccache --show-stats", step(job, "Show ccache statistics")["run"])
        self.assertLess(names.index("Configure ccache"), names.index("Restore ccache"))
        self.assertLess(names.index("Restore ccache"), names.index("Build Windows"))
        self.assertEqual(names.index("Save ccache"), names.index("Build Windows") + 1)

    def test_debug_info_only_when_symbols_are_packaged(self) -> None:
        job = build_job()
        self.assertEqual(
            job["env"]["WITH_SYMBOLS"],
            "${{ startsWith(github.ref, 'refs/tags/') || inputs.publish_symbols || inputs.upload_symbols_artifact }}",
        )
        build = step(job, "Build Windows")["run"]
        self.assertIn('release_flags="-O3 -DNDEBUG"', build)
        self.assertIn('if [[ "${WITH_SYMBOLS}" == "true" ]]; then\n  release_flags+=" -g -gdwarf-4"', build)
        self.assertEqual(build.count("-gdwarf-4"), 1)
        for name in ("Install cv2pdb", "Generate Windows PDB", "Package Windows Symbols", "Upload Windows symbols package"):
            self.assertEqual(step(job, name)["if"], "env.WITH_SYMBOLS == 'true'")

    def test_tests_run_after_the_build(self) -> None:
        job = build_job()
        names = [s["name"] for s in job["steps"]]
        tests = step(job, "Run tests")
        self.assertEqual(tests["if"], "inputs.build_tests")
        self.assertRegex(tests["run"], r"^ctest --test-dir build --output-on-failure --timeout \d+ ")
        amf = step(job, "Test native AMF lifecycle behavior")
        self.assertEqual(amf["if"], "inputs.build_tests")
        self.assertIn("-DSUNSHINE_AMF_LIFECYCLE_STANDALONE", amf["run"])
        self.assertLess(names.index("Build Windows"), names.index("Run tests"))
        self.assertLess(names.index("Run tests"), names.index("Package Windows MSI"))
        contracts = step(job, "Test CI workflow contracts")
        self.assertEqual(contracts["run"], "python .github/scripts/test_ci_workflows.py")

    def test_build_parallelism_is_bounded(self) -> None:
        job = build_job()
        self.assertEqual(job["env"]["CMAKE_BUILD_PARALLEL_LEVEL"], "6")
        scripts = "\n".join(s.get("run", "") for s in job["steps"])
        self.assertNotIn("$(nproc)", scripts)
        commands = [line for line in scripts.splitlines() if "cmake --build " in line]
        self.assertEqual(len(commands), 2)
        for command in commands:
            self.assertIn('--parallel "$CMAKE_BUILD_PARALLEL_LEVEL"', command)

    def test_vhf_cmake_pins_match_the_downloaded_release(self) -> None:
        env = build_job()["env"]
        build = step(build_job(), "Build Windows")["run"]
        pins = dict(re.findall(r"-DSUNSHINE_VHF_GAMEPAD_([A-Z0-9_]+)=(\S+)", build))
        self.assertEqual(pins["RELEASE_TAG"], env["VHF_TAG"])
        self.assertEqual(pins["RELEASE_ASSET_SHA256"], env["VHF_ARCHIVE_SHA256"])
        self.assertEqual(pins["SOURCE_REVISION"], env["VHF_SOURCE_REVISION"])
        self.assertEqual(pins["DRIVER_VER"], env["VHF_DRIVER_VER"])
        self.assertEqual(pins["PROTOCOL_VERSION"], env["VHF_PROTOCOL_VERSION"])

    def test_windows_builds_and_ships_pinned_pyrowave(self) -> None:
        job = build_job()
        names = [s["name"] for s in job["steps"]]

        script = (ROOT / "scripts" / "build_pyrowave.sh").read_text(encoding="utf-8")
        self.assertIn(f"PINNED_COMMIT={job['env']['PYROWAVE_COMMIT']}\n", script)

        cache = step(job, "Cache PyroWave")
        self.assertTrue(cache["uses"].startswith("actions/cache@"))
        self.assertIn("${{ env.PYROWAVE_COMMIT }}", cache["with"]["key"])
        self.assertIn("hashFiles('scripts/build_pyrowave.sh')", cache["with"]["key"])

        build_pyrowave = step(job, "Build PyroWave")
        self.assertEqual(build_pyrowave["if"], "steps.cache-pyrowave.outputs.cache-hit != 'true'")
        self.assertIn('bash scripts/build_pyrowave.sh "${PYROWAVE_COMMIT}"', build_pyrowave["run"])
        self.assertLess(names.index("Build PyroWave"), names.index("Build Windows"))

        build = step(job, "Build Windows")["run"]
        self.assertIn("-DSUNSHINE_ENABLE_PYROWAVE=ON", build)
        self.assertIn("-DSUNSHINE_PYROWAVE_ROOT=", build)

        verify = step(job, "Verify unsigned MSI ships the PyroWave runtime")
        self.assertIn("libpyrowave-shared-0.dll", verify["run"])
        self.assertLess(names.index("Package Windows MSI"), names.index("Verify unsigned MSI ships the PyroWave runtime"))


class ReleaseVersionTest(unittest.TestCase):
    def test_explicit_build_version_does_not_require_branch_context(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_dir:
            probe = Path(temporary_dir) / "probe-version.cmake"
            probe.write_text(
                f'include("{(ROOT / "cmake" / "prep" / "build_version.cmake").as_posix()}")\n'
                'if(NOT PROJECT_VERSION_FULL STREQUAL "1.19.0-beta.5")\n'
                '  message(FATAL_ERROR "explicit cache version was not retained: ${PROJECT_VERSION_FULL}")\n'
                'endif()\n'
                'if(NOT PROJECT_VERSION_NUMERIC STREQUAL "1.19.0")\n'
                '  message(FATAL_ERROR "numeric version was not split correctly: ${PROJECT_VERSION_NUMERIC}")\n'
                'endif()\n',
                encoding="utf-8",
            )
            environment = os.environ.copy()
            environment.pop("BUILD_VERSION", None)
            environment.pop("BRANCH", None)

            def run(version: str, env: dict) -> subprocess.CompletedProcess:
                return subprocess.run(
                    ["cmake", f"-DBUILD_VERSION={version}", "-P", str(probe)],
                    cwd=ROOT, env=env, check=False, capture_output=True, text=True,
                )

            self.assertEqual(run("1.19.0-beta.5", environment).returncode, 0)
            self.assertEqual(run("1.19.0-beta.5", {**environment, "BUILD_VERSION": ""}).returncode, 0)

            invalid = run("1.19", environment)
            self.assertNotEqual(invalid.returncode, 0)
            self.assertIn("Invalid Vibeshine build version", invalid.stderr)

            zero = run("0.0.0", environment)
            self.assertNotEqual(zero.returncode, 0)
            self.assertIn("Version resolution produced 0.0.0", zero.stderr)

    def test_stable_respins_keep_the_stable_windows_version_ordinal(self) -> None:
        wix_version = (ROOT / "cmake" / "packaging" / "windows_wix.cmake").read_text(encoding="utf-8")
        executable_version = (ROOT / "cmake" / "prep" / "emit_windows_versioninfo.cmake").read_text(encoding="utf-8")
        bootstrapper = (ROOT / "packaging" / "windows" / "bootstrapper" / "VibeshineInstaller.cs").read_text(encoding="utf-8")

        self.assertRegex(wix_version, r'elseif\(_pre_tag STREQUAL "stable"\)\n(?:\s*#.*\n)*\s*set\(_WIX_PRERELEASE_ORDINAL 99\)')
        self.assertRegex(executable_version, r'elseif\("\$\{_pre_tag\}" STREQUAL "stable"\)\n(?:\s*#.*\n)*\s*set\(_ordinal 99\)')
        self.assertRegex(bootstrapper, r'if \(string\.Equals\(tag, "stable", StringComparison\.Ordinal\)\) \{\n(?:\s*//.*\n)*\s*return 99;')


if __name__ == "__main__":
    unittest.main()
