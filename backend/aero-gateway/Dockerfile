# syntax=docker/dockerfile:1
FROM node:20-alpine AS build

WORKDIR /app

COPY package.json package-lock.json ./
RUN npm ci

COPY . .

ARG GIT_SHA=dev
ENV AERO_GATEWAY_VERSION=$GIT_SHA

RUN npm run build

FROM node:20-alpine AS runtime

WORKDIR /app

ENV NODE_ENV=production

COPY package.json package-lock.json ./
RUN npm ci --omit=dev

COPY --from=build /app/dist ./dist

EXPOSE 8080

CMD ["node", "dist/index.js"]
