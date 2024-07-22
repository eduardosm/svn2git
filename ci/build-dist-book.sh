#!/usr/bin/env bash
set -euo pipefail

cd book

mdbook build
mv book svn2git-book
zip -r svn2git-book.zip svn2git-book
