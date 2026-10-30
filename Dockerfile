FROM alpine:3.24@sha256:be2656061b536c1883c4034790e21d0a09c5aaaf51968b89368a3c0371ea2c9a

ARG TARGETARCH

# bash and curl run install.sh, which fetches releases for agent upgrades, SSH
# provisioning and server updates.
RUN apk add --no-cache bash ca-certificates curl tzdata \
    && addgroup -S -g 10001 redoor \
    && adduser -S -D -u 10001 -G redoor -h /home/redoor redoor \
    && mkdir -p /etc/redoor /home/redoor/.local/share/redoor \
    && chown -R 10001:10001 /home/redoor

COPY target/container/${TARGETARCH}/redoor /usr/local/bin/redoor
COPY --chown=10001:10001 docker/demo-config.toml /etc/redoor/config.toml

ENV HOME=/home/redoor

USER 10001:10001
WORKDIR /home/redoor

EXPOSE 7666

ENTRYPOINT ["/usr/local/bin/redoor"]
CMD ["server", "--config", "/etc/redoor/config.toml"]
