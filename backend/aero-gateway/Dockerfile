# syntax=docker/dockerfile:1
ARG NODE_VERSION=20.11.1
FROM node:${NODE_VERSION}-alpine AS build

ARG GIT_SHA=dev
ARG VERSION=$GIT_SHA
ARG BUILD_TIMESTAMP=unknown
ENV AERO_GATEWAY_VERSION=$VERSION
ENV AERO_GATEWAY_GIT_SHA=$GIT_SHA
ENV AERO_GATEWAY_BUILD_TIMESTAMP=$BUILD_TIMESTAMP

WORKDIR /repo

# This Dockerfile expects the *repository root* as its build context so it can
# use the monorepo `package-lock.json` and shared devDependencies (TypeScript, etc.).
#
# Example:
#   docker build -f backend/aero-gateway/Dockerfile -t aero-gateway .
#
# Copy package manifests first to maximize layer caching.
COPY package.json package-lock.json ./
COPY backend/aero-gateway/package.json backend/aero-gateway/package.json

RUN npm ci -w backend/aero-gateway --include-workspace-root --no-audit --no-fund

COPY backend/aero-gateway backend/aero-gateway

RUN npm -w backend/aero-gateway run build

RUN set -eux; \
    mkdir -p /out/backend/aero-gateway; \
    cp package.json package-lock.json /out/; \
    cp backend/aero-gateway/package.json /out/backend/aero-gateway/package.json; \
    cp -r backend/aero-gateway/dist /out/backend/aero-gateway/dist

FROM node:${NODE_VERSION}-alpine AS runtime
WORKDIR /repo

ENV NODE_ENV=production

ARG GIT_SHA=dev
ARG VERSION=$GIT_SHA
ARG BUILD_TIMESTAMP=unknown
ENV AERO_GATEWAY_VERSION=$VERSION
ENV AERO_GATEWAY_GIT_SHA=$GIT_SHA
ENV AERO_GATEWAY_BUILD_TIMESTAMP=$BUILD_TIMESTAMP

COPY --from=build /out/package.json /out/package-lock.json ./
COPY --from=build /out/backend/aero-gateway/package.json backend/aero-gateway/package.json

RUN npm ci -w backend/aero-gateway --include-workspace-root --omit=dev --no-audit --no-fund

COPY --from=build /out/backend/aero-gateway/dist backend/aero-gateway/dist

EXPOSE 8080

USER node

WORKDIR /repo/backend/aero-gateway

CMD ["node", "dist/index.js"]
