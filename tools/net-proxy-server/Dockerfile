# syntax=docker/dockerfile:1
FROM node:22.11.0-alpine

WORKDIR /repo

# This Dockerfile expects the repository root as its build context so it can use
# the monorepo `package-lock.json` for deterministic installs.
#
# Example:
#   docker build -f tools/net-proxy-server/Dockerfile -t aero-net-proxy-server .
COPY package.json package-lock.json ./
COPY tools/net-proxy-server/package.json tools/net-proxy-server/package.json

RUN npm ci -w tools/net-proxy-server --omit=dev --no-audit --no-fund

COPY tools/net-proxy-server/src tools/net-proxy-server/src

ENV AERO_PROXY_HOST=0.0.0.0
ENV AERO_PROXY_PORT=8080

EXPOSE 8080

WORKDIR /repo/tools/net-proxy-server
CMD ["node", "src/cli.js"]
