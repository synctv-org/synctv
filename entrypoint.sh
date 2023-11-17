#!/bin/bash

chown -R ${PUID}:${PGID} /opt/synctv

umask ${UMASK}

exec su-exec ${PUID}:${PGID} synctv server --env-no-prefix
