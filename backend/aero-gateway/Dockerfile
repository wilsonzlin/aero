# syntax=docker/dockerfile:1
FROM node:20-alpine AS build

ARG GIT_SHA=dev
ENV AERO_GATEWAY_VERSION=$GIT_SHA

WORKDIR /repo
COPY . .

RUN set -eux; \
    if [ -f package.json ] && grep -q '"name": "aero-gateway"' package.json; then \
      SERVICE_DIR="."; \
    elif [ -f backend/aero-gateway/package.json ] && grep -q '"name": "aero-gateway"' backend/aero-gateway/package.json; then \
      SERVICE_DIR="backend/aero-gateway"; \
    else \
      echo "aero-gateway package.json not found in build context"; \
      exit 1; \
    fi; \
    cd "$SERVICE_DIR"; \
    npm ci; \
    npm run build; \
    mkdir -p /out; \
    cp package.json package-lock.json /out/; \
    cp -r dist /out/dist

FROM node:20-alpine AS runtime
WORKDIR /app

ENV NODE_ENV=production

ARG GIT_SHA=dev
ENV AERO_GATEWAY_VERSION=$GIT_SHA

COPY --from=build /out/package.json /out/package-lock.json ./
RUN npm ci --omit=dev

COPY --from=build /out/dist ./dist

EXPOSE 8080

USER node

CMD ["node", "dist/index.js"]
