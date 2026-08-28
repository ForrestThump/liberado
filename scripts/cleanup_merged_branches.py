#!/usr/bin/env python3
"""Safely remove local and remote branches whose content is on a base branch.

The default is a dry run. Apply mode fetches and audits again, creates recovery
tags, verifies every exact ref tip, and only then deletes. The tool is separate
from the Liberado binary on purpose: repository cleanup must not be implemented
by the program whose own branch can be a cleanup target.
"""

from __future__ import annotations

import argparse
import dataclasses
import datetime as dt
import pathlib
import subprocess
import sys
from collections.abc import Iterable, Sequence


class GitError(RuntimeError):
    """A Git command failed."""


@dataclasses.dataclass(frozen=True)
class Candidate:
    scope: str
    branch: str
    ref: str
    oid: str
    reason: str


@dataclasses.dataclass
class Audit:
    delete: list[Candidate] = dataclasses.field(default_factory=list)
    keep: list[tuple[str, str]] = dataclasses.field(default_factory=list)
    skip: list[tuple[str, str]] = dataclasses.field(default_factory=list)


class Cleaner:
    def __init__(self, repo: pathlib.Path, remote: str, base: str | None) -> None:
        self.repo = repo.resolve()
        self.remote = remote
        self.base = base

    def git(
        self,
        *args: str,
        check: bool = True,
        capture: bool = True,
    ) -> subprocess.CompletedProcess[str]:
        result = subprocess.run(
            ["git", *args],
            cwd=self.repo,
            check=False,
            text=True,
            stdout=subprocess.PIPE if capture else None,
            stderr=subprocess.PIPE if capture else None,
        )
        if check and result.returncode != 0:
            detail = (result.stderr or result.stdout or "").strip()
            raise GitError(f"git {' '.join(args)} failed ({result.returncode}): {detail}")
        return result

    def fetch(self) -> None:
        self.git("fetch", self.remote, "--prune", capture=False)

    def ref_oid(self, ref: str) -> str | None:
        result = self.git("rev-parse", "--verify", "--quiet", f"{ref}^{{commit}}", check=False)
        return result.stdout.strip() if result.returncode == 0 else None

    def resolve_base(self) -> str:
        if self.base:
            base = self.base
        elif self.ref_oid(f"refs/remotes/{self.remote}/main"):
            base = f"{self.remote}/main"
        else:
            base = "main"
        if not self.ref_oid(base):
            raise GitError(f"base ref {base!r} does not exist; fetch or pass --base")
        self.base = base
        return base

    def is_ancestor(self, ref: str, base: str) -> bool:
        return self.git("merge-base", "--is-ancestor", ref, base, check=False).returncode == 0

    def tree_oid(self, ref: str) -> str:
        return self.git("rev-parse", "--verify", f"{ref}^{{tree}}").stdout.strip()

    def content_is_on_base(self, ref: str, base: str) -> bool:
        result = self.git("merge-tree", "--write-tree", base, ref, check=False)
        if result.returncode != 0 or not result.stdout.strip():
            return False
        merged_tree = result.stdout.splitlines()[0].strip()
        return merged_tree == self.tree_oid(base)

    def safe_reason(self, ref: str, base: str) -> str | None:
        if self.is_ancestor(ref, base):
            return f"tip is an ancestor of {base}"
        if self.content_is_on_base(ref, base):
            return f"merge result equals {base} tree (squash or equivalent)"
        return None

    def refs(self, prefix: str) -> list[str]:
        output = self.git("for-each-ref", "--format=%(refname)", prefix).stdout
        return [line for line in output.splitlines() if line]

    def worktree_branches(self) -> dict[str, str]:
        branches: dict[str, str] = {}
        path = ""
        for line in self.git("worktree", "list", "--porcelain").stdout.splitlines():
            if line.startswith("worktree "):
                path = line.removeprefix("worktree ")
            elif line.startswith("branch refs/heads/"):
                branches[line.removeprefix("branch refs/heads/")] = path
        return branches

    def current_branch(self) -> str:
        return self.git("branch", "--show-current").stdout.strip()

    def remote_head(self) -> str | None:
        result = self.git(
            "symbolic-ref", "--quiet", "--short", f"refs/remotes/{self.remote}/HEAD", check=False
        )
        if result.returncode != 0:
            return None
        short = result.stdout.strip()
        prefix = f"{self.remote}/"
        return short.removeprefix(prefix)

    def ahead_count(self, ref: str, base: str) -> str:
        result = self.git("rev-list", "--count", f"{base}..{ref}", check=False)
        return result.stdout.strip() if result.returncode == 0 else "?"

    def audit(self, *, local_only: bool = False) -> Audit:
        base = self.resolve_base()
        audit = Audit()
        current = self.current_branch()
        worktrees = self.worktree_branches()
        protected = {"main", "master"}
        if head := self.remote_head():
            protected.add(head)

        for ref in self.refs("refs/heads"):
            branch = ref.removeprefix("refs/heads/")
            label = f"local/{branch}"
            if branch in protected:
                audit.skip.append((label, "protected"))
            elif branch == current:
                audit.skip.append((label, "current branch"))
            elif branch in worktrees:
                audit.skip.append((label, f"checked out in {worktrees[branch]}"))
            elif reason := self.safe_reason(ref, base):
                oid = self.ref_oid(ref)
                if not oid:
                    raise GitError(f"candidate ref disappeared: {ref}")
                audit.delete.append(Candidate("local", branch, ref, oid, reason))
            else:
                audit.keep.append((label, f"{self.ahead_count(ref, base)} commit(s) not on {base}"))

        if local_only:
            return audit

        prefix = f"refs/remotes/{self.remote}/"
        for ref in self.refs(prefix):
            branch = ref.removeprefix(prefix)
            label = f"{self.remote}/{branch}"
            if branch == "HEAD" or branch in protected:
                audit.skip.append((label, "protected"))
            elif branch == current or branch in worktrees:
                audit.skip.append((label, "corresponding local branch is checked out"))
            elif reason := self.safe_reason(ref, base):
                oid = self.ref_oid(ref)
                if not oid:
                    raise GitError(f"candidate ref disappeared: {ref}")
                audit.delete.append(Candidate("remote", branch, ref, oid, reason))
            else:
                audit.keep.append((label, f"{self.ahead_count(ref, base)} commit(s) not on {base}"))
        return audit

    def recheck(self, candidates: Iterable[Candidate]) -> None:
        base = self.resolve_base()
        current = self.current_branch()
        worktrees = self.worktree_branches()
        for candidate in candidates:
            actual = self.ref_oid(candidate.ref)
            if actual != candidate.oid:
                raise GitError(
                    f"{candidate.ref} moved from {candidate.oid} to {actual}; refusing deletion"
                )
            if candidate.branch == current or candidate.branch in worktrees:
                raise GitError(f"{candidate.branch} is now checked out; refusing deletion")
            if not self.safe_reason(candidate.ref, base):
                raise GitError(f"{candidate.ref} no longer passes the content safety check")

    def archive(self, candidates: Iterable[Candidate]) -> None:
        date = dt.date.today().isoformat()
        for candidate in candidates:
            short = candidate.oid[:12]
            tag = f"archive/stale-branches/{date}/{candidate.scope}/{candidate.branch}-{short}"
            tag_ref = f"refs/tags/{tag}"
            existing = self.ref_oid(tag_ref)
            if existing and existing != candidate.oid:
                raise GitError(f"archive tag {tag!r} exists at a different commit")
            if not existing:
                self.git("tag", tag, candidate.oid)
            self.git("push", self.remote, f"{tag_ref}:{tag_ref}", capture=False)
            print(f"Archived {candidate.ref} as {tag}")

    def delete(self, candidates: Iterable[Candidate]) -> None:
        failures = 0
        for candidate in candidates:
            if candidate.scope == "remote":
                lease = f"--force-with-lease=refs/heads/{candidate.branch}:{candidate.oid}"
                result = self.git(
                    "push", self.remote, lease, "--delete", candidate.branch,
                    check=False, capture=False,
                )
            else:
                if self.ref_oid(candidate.ref) != candidate.oid:
                    print(f"FAILED: {candidate.ref} moved before deletion", file=sys.stderr)
                    failures += 1
                    continue
                result = self.git("branch", "-D", "--", candidate.branch, check=False, capture=False)
            if result.returncode != 0:
                print(f"FAILED to delete {candidate.ref}", file=sys.stderr)
                failures += 1
        if failures:
            raise GitError(f"{failures} branch deletion(s) failed")


def print_section(title: str, items: Sequence[tuple[str, str]]) -> None:
    if not items:
        return
    print(f"\n{title}")
    for label, detail in items:
        print(f"  {label:<58} {detail}")


def print_audit(cleaner: Cleaner, audit: Audit, *, apply: bool) -> None:
    base = cleaner.resolve_base()
    print(f"Base: {base} ({cleaner.ref_oid(base)})")
    if not apply:
        print("Dry run. Pass --apply to archive and delete.")
    print_section(
        "Eligible for deletion:",
        [(f"{item.scope}/{item.branch}", f"{item.reason}; tip {item.oid[:12]}") for item in audit.delete],
    )
    print_section("Keep (has content not on the base):", audit.keep)
    print_section("Skip:", audit.skip)


def parse_args(argv: Sequence[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--apply", action="store_true", help="archive and delete safe candidates")
    parser.add_argument("--no-fetch", action="store_true", help="use existing refs without fetching")
    parser.add_argument("--local-only", action="store_true", help="do not audit remote branches")
    parser.add_argument("--no-archive", action="store_true", help="delete without recovery tags")
    parser.add_argument("--base", help="base ref; defaults to <remote>/main, then main")
    parser.add_argument("--remote", default="origin")
    parser.add_argument("--repo", type=pathlib.Path, default=pathlib.Path.cwd())
    return parser.parse_args(argv)


def main(argv: Sequence[str] | None = None) -> int:
    args = parse_args(argv or sys.argv[1:])
    cleaner = Cleaner(args.repo, args.remote, args.base)
    try:
        if not args.no_fetch:
            print(f"Fetching {args.remote} and pruning tracking refs...")
            cleaner.fetch()
        audit = cleaner.audit(local_only=args.local_only)
        print_audit(cleaner, audit, apply=args.apply)
        if not audit.delete:
            print("\nNothing to delete.")
            return 0
        if not args.apply:
            print(f"\n{len(audit.delete)} branch ref(s) would be archived and deleted.")
            return 0
        cleaner.recheck(audit.delete)
        if not args.no_archive:
            cleaner.archive(audit.delete)
        cleaner.delete(audit.delete)
        print(f"Deleted {len(audit.delete)} branch ref(s).")
        return 0
    except GitError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
