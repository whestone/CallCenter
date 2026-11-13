#!/bin/sh
set -e

# Ohne DATABASE_URL: eingebaute PostgreSQL, bei jedem Start leer → frische Demodaten.
if [ -z "$DATABASE_URL" ]; then
    PGBIN=$(ls -d /usr/lib/postgresql/*/bin | head -n 1)
    PGDATA=/tmp/pgdata
    rm -rf "$PGDATA"
    mkdir -p "$PGDATA"
    chown postgres "$PGDATA"
    runuser -u postgres -- "$PGBIN/initdb" -D "$PGDATA" -U postgres --auth=trust -E UTF8 --locale=C.UTF-8 >/dev/null
    runuser -u postgres -- "$PGBIN/pg_ctl" -D "$PGDATA" -l /tmp/postgres.log -w start \
        -o "-c listen_addresses=127.0.0.1 -c unix_socket_directories=/tmp -c shared_buffers=32MB -c max_connections=30" >/dev/null
    runuser -u postgres -- "$PGBIN/createdb" -h 127.0.0.1 -U postgres callcenter
    export DATABASE_URL=postgres://postgres@127.0.0.1:5432/callcenter
fi

export BIND_ADDR="0.0.0.0:${PORT:-3000}"
# Render liefert die eigene Adresse mit
[ -z "$PUBLIC_URL" ] && [ -n "$RENDER_EXTERNAL_URL" ] && export PUBLIC_URL="$RENDER_EXTERNAL_URL"

exec /app/callcenter
