"""Verify the resolved product graphs, not optional dependencies kept in Cargo.lock."""
import pathlib
import subprocess
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[1]


class FixedFeatureIsolation(unittest.TestCase):
    def tree(self, *features):
        result = subprocess.run(["cargo", "tree", "--locked", "--edges", "normal", "--prefix", "none",
            "--format", "{p}", *features], cwd=ROOT, text=True, capture_output=True, check=True)
        return {line.split()[0] for line in result.stdout.splitlines() if line.strip()}

    def test_n2_graph_excludes_candle_and_download_engine(self):
        packages = self.tree("--no-default-features", "--features", "fixed-local-models",
                             "--target", "riscv64gc-unknown-linux-gnu")
        self.assertFalse(any(name.startswith("candle-") or name == "hf-hub" for name in packages))
        self.assertIn("tokenizers", packages)

    def test_n1_keeps_candle_and_model_management(self):
        packages = self.tree()
        self.assertTrue({"candle-core", "candle-nn", "candle-transformers", "hf-hub"} <= packages)


if __name__ == "__main__":
    unittest.main()
