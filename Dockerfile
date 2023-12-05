From alpine:latest as builder

ARG VERSION=v0.0.0

WORKDIR /synctv

COPY ./ ./

RUN apk add --no-cache bash curl gcc git go musl-dev && \
    bash script/build.sh -P -v ${VERSION}

From alpine:latest

ENV PUID=0 PGID=0 UMASK=022

COPY --from=builder /synctv/build/synctv /usr/local/bin/synctv

COPY script/entrypoint.sh /entrypoint.sh

RUN apk add --no-cache bash ca-certificates su-exec tzdata && \
    rm -rf /var/cache/apk/* && \
    chmod +x /entrypoint.sh && \
    mkdir -p ~/.synctv

WORKDIR ~/.synctv

EXPOSE 8080/tcp 8080/udp

VOLUME [ "~/.synctv" ]

CMD [ "/entrypoint.sh" ]
