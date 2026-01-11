FROM node:22.11.0-alpine AS build

WORKDIR /repo

COPY . .
ENV PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1

# Install production deps for the legacy server workspace.
RUN npm ci --omit=dev -w server

RUN mkdir -p /out \
  && cp -r node_modules /out/node_modules \
  && cp -r server/public /out/public \
  && cp -r server/src /out/src

FROM node:22.11.0-alpine

WORKDIR /app

COPY --from=build /out/node_modules ./node_modules
COPY --from=build /out/public ./public
COPY --from=build /out/src ./src

ENV AERO_PROXY_HOST=0.0.0.0
ENV AERO_PROXY_PORT=8080

EXPOSE 8080

CMD ["node", "src/index.js"]
