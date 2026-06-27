#!/usr/bin/env bash
# Prove pgsafe's rules against real Postgres across a version matrix.
# Spins up a throwaway Postgres per version, runs the #[ignore]d proof tests, reports.
# Usage: scripts/prove-rules.sh [VERSION ...]   (default: 14 15 16 17 18)
set -euo pipefail

versions=("$@")
[ "${#versions[@]}" -eq 0 ] && versions=(14 15 16 17 18)

port=55459
pass=secret
failed=()

for v in "${versions[@]}"; do
  name="pgsafe_proof_${v}"
  echo "=== PostgreSQL ${v} ==="
  docker rm -f "$name" >/dev/null 2>&1 || true
  docker run -d --name "$name" -e POSTGRES_PASSWORD="$pass" -p "${port}:5432" "postgres:${v}" >/dev/null
  for _ in $(seq 1 30); do
    docker exec "$name" pg_isready -U postgres >/dev/null 2>&1 && break
    sleep 1
  done
  if DATABASE_URL="postgres://postgres:${pass}@127.0.0.1:${port}/postgres" \
       cargo test --test rule_proofs -- --ignored --nocapture; then
    echo "PostgreSQL ${v}: OK"
  else
    echo "PostgreSQL ${v}: FAIL"
    failed+=("$v")
  fi
  docker rm -f "$name" >/dev/null 2>&1 || true
done

if [ "${#failed[@]}" -gt 0 ]; then
  echo "FAILED versions: ${failed[*]}"
  exit 1
fi
echo "All rules proven across: ${versions[*]}"
