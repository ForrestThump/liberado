from __future__ import annotations

import importlib.util
import io
import pathlib
import subprocess
import sys
import tempfile
import unittest
from contextlib import redirect_stdout


SCRIPT = pathlib.Path(__file__).with_name("cleanup_merged_branches.py")
SPEC = importlib.util.spec_from_file_location("cleanup_merged_branches", SCRIPT)
assert SPEC and SPEC.loader
cleanup = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = cleanup
SPEC.loader.exec_module(cleanup)


def git(repo: pathlib.Path, *args: str) -> str:
    return subprocess.run(
        ["git", *args], cwd=repo, check=True, text=True, capture_output=True
    ).stdout.strip()


class CleanerTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.repo = pathlib.Path(self.temp.name) / "repo"
        self.repo.mkdir()
        git(self.repo, "init", "-b", "main")
        git(self.repo, "config", "user.email", "test@example.invalid")
        git(self.repo, "config", "user.name", "Branch Cleaner Test")
        (self.repo / "base.txt").write_text("base\n", encoding="utf-8")
        git(self.repo, "add", "base.txt")
        git(self.repo, "commit", "-m", "base")

    def tearDown(self) -> None:
        self.temp.cleanup()

    def commit_on(self, branch: str, path: str, text: str) -> str:
        git(self.repo, "switch", "-c", branch)
        (self.repo / path).write_text(text, encoding="utf-8")
        git(self.repo, "add", path)
        git(self.repo, "commit", "-m", branch)
        return git(self.repo, "rev-parse", "HEAD")

    def cleaner(self) -> cleanup.Cleaner:
        return cleanup.Cleaner(self.repo, "origin", "main")

    def add_remote(self) -> pathlib.Path:
        remote = pathlib.Path(self.temp.name) / "remote.git"
        subprocess.run(
            ["git", "init", "--bare", str(remote)],
            cwd=self.temp.name,
            check=True,
            text=True,
            capture_output=True,
        )
        git(self.repo, "remote", "add", "origin", str(remote))
        git(self.repo, "push", "-u", "origin", "main")
        return remote

    def remote_oid(self, remote: pathlib.Path, ref: str) -> str | None:
        result = subprocess.run(
            ["git", "--git-dir", str(remote), "rev-parse", "--verify", "--quiet", ref],
            check=False,
            text=True,
            capture_output=True,
        )
        return result.stdout.strip() if result.returncode == 0 else None

    def remote_candidate(
        self,
    ) -> tuple[pathlib.Path, cleanup.Cleaner, cleanup.Candidate]:
        merged_oid = self.commit_on("merged", "merged.txt", "merged\n")
        git(self.repo, "switch", "main")
        git(self.repo, "merge", "--no-ff", "merged", "-m", "merge merged")
        remote = self.add_remote()
        git(self.repo, "push", "origin", "merged")
        cleaner = cleanup.Cleaner(self.repo, "origin", "origin/main")
        audit = cleaner.audit()
        candidate = next(
            item
            for item in audit.delete
            if item.scope == "remote" and item.branch == "merged"
        )
        self.assertEqual(candidate.oid, merged_oid)
        return remote, cleaner, candidate

    def test_dry_audit_selects_only_merged_content(self) -> None:
        merged_oid = self.commit_on("merged", "merged.txt", "merged\n")
        git(self.repo, "switch", "main")
        git(self.repo, "merge", "--no-ff", "merged", "-m", "merge merged")
        self.commit_on("unique", "unique.txt", "unique\n")
        git(self.repo, "switch", "main")

        audit = self.cleaner().audit(local_only=True)

        self.assertEqual([(c.branch, c.oid) for c in audit.delete], [("merged", merged_oid)])
        self.assertTrue(any(label == "local/unique" for label, _ in audit.keep))
        self.assertTrue(any(label == "local/main" for label, _ in audit.skip))
        self.assertEqual(git(self.repo, "rev-parse", "merged"), merged_oid)

    def test_content_equivalent_squash_branch_is_selected(self) -> None:
        self.commit_on("squashed", "squashed.txt", "same content\n")
        git(self.repo, "switch", "main")
        git(self.repo, "merge", "--squash", "squashed")
        git(self.repo, "commit", "-m", "squash")

        audit = self.cleaner().audit(local_only=True)

        item = next(c for c in audit.delete if c.branch == "squashed")
        self.assertIn("squash or equivalent", item.reason)

    def test_apply_rechecks_tip_and_deletes_expected_branch(self) -> None:
        merged_oid = self.commit_on("merged", "merged.txt", "merged\n")
        git(self.repo, "switch", "main")
        git(self.repo, "merge", "--no-ff", "merged", "-m", "merge merged")
        cleaner = self.cleaner()
        audit = cleaner.audit(local_only=True)

        cleaner.recheck(audit.delete)
        cleaner.delete(audit.delete)

        self.assertIsNone(cleaner.ref_oid("refs/heads/merged"))
        self.assertEqual(git(self.repo, "rev-parse", "main^{commit}"), git(self.repo, "rev-parse", "HEAD"))
        self.assertTrue(merged_oid)

    def test_recheck_refuses_a_moved_candidate(self) -> None:
        self.commit_on("merged", "merged.txt", "merged\n")
        git(self.repo, "switch", "main")
        git(self.repo, "merge", "--no-ff", "merged", "-m", "merge merged")
        cleaner = self.cleaner()
        audit = cleaner.audit(local_only=True)
        git(self.repo, "branch", "-f", "merged", "main")

        with self.assertRaises(cleanup.GitError):
            cleaner.recheck(audit.delete)

    def test_remote_dry_run_does_not_change_refs(self) -> None:
        remote, _, candidate = self.remote_candidate()

        output = io.StringIO()
        with redirect_stdout(output):
            result = cleanup.main(
                [
                    "--no-fetch",
                    "--repo",
                    str(self.repo),
                    "--base",
                    "origin/main",
                ]
            )

        self.assertEqual(result, 0)
        self.assertIn("Dry run", output.getvalue())
        self.assertEqual(self.remote_oid(remote, "refs/heads/merged"), candidate.oid)
        self.assertIsNone(self.remote_oid(remote, "refs/tags/archive/stale-branches"))

    def test_remote_apply_archives_exact_tip_before_deletion(self) -> None:
        remote, cleaner, candidate = self.remote_candidate()

        cleaner.recheck([candidate])
        cleaner.archive([candidate])
        cleaner.delete([candidate])

        self.assertIsNone(self.remote_oid(remote, "refs/heads/merged"))
        tags = subprocess.run(
            [
                "git",
                "--git-dir",
                str(remote),
                "for-each-ref",
                "--format=%(refname) %(objectname)",
                "refs/tags/archive/stale-branches/",
            ],
            check=True,
            text=True,
            capture_output=True,
        ).stdout.splitlines()
        self.assertEqual(len(tags), 1)
        tag_ref, tag_oid = tags[0].split()
        self.assertIn("/remote/merged-", tag_ref)
        self.assertEqual(tag_oid, candidate.oid)

    def test_remote_delete_lease_refuses_a_moved_tip(self) -> None:
        remote, cleaner, candidate = self.remote_candidate()
        moved_oid = self.commit_on("moved", "moved.txt", "moved\n")
        git(self.repo, "push", "origin", "moved")
        subprocess.run(
            [
                "git",
                "--git-dir",
                str(remote),
                "update-ref",
                "refs/heads/merged",
                moved_oid,
            ],
            check=True,
            text=True,
            capture_output=True,
        )

        with self.assertRaises(cleanup.GitError):
            cleaner.delete([candidate])

        self.assertEqual(self.remote_oid(remote, "refs/heads/merged"), moved_oid)


if __name__ == "__main__":
    unittest.main()
