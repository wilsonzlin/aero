# syntax=docker/dockerfile:1
FROM node:20-alpine AS build

WORKDIR /app/backend/aero-gateway

COPY backend/aero-gateway/package.json backend/aero-gateway/package-lock.json ./
RUN npm ci

COPY backend/aero-gateway/ ./

ARG GIT_SHA=dev
ENV AERO_GATEWAY_VERSION=$GIT_SHA

RUN npm run build

FROM node:20-alpine AS runtime

WORKDIR /app/backend/aero-gateway

ENV NODE_ENV=production

COPY backend/aero-gateway/package.json backend/aero-gateway/package-lock.json ./
RUN npm ci --omit=dev

COPY --from=build /app/backend/aero-gateway/dist ./dist

EXPOSE 8080

CMD ["node", "dist/index.js"]

