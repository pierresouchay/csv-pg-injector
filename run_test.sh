#!/bin/bash

set -eu

PG_VERSION=${1:-17}

cleanup() {
  docker rm --force --volumes postgres-test && echo 'stopping postgres...' || echo ''
}

cleanup

docker run --name postgres-test \
  --health-start-interval 100ms --health-start-period 10s --health-cmd pg_isready \
  -d -e POSTGRES_PASSWORD=pgpasswd \
  -e POSTGRES_USER=pguser -e POSTGRES_DB=pgdb -p 15432:5432 \
  "postgres:${PG_VERSION}"

printf "Wait for PG to be up..."
status=$(docker inspect --format '{{.State.Health.Status}}' postgres-test)
while [ "healthy" != "$status" ]
do
  printf "%s..." "$status"
  sleep 1
  status=$(docker inspect --format '{{.State.Health.Status}}' postgres-test)
done
echo "[OK] PG is ready"

trap "cleanup" 2 3 15

DATABASE_URL=postgresql://pguser:pgpasswd@localhost:15432/pgdb cargo test

docker stop postgres-test
docker rm --force --volumes postgres-test