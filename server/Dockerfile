FROM node:22-alpine

WORKDIR /app

COPY package.json package-lock.json ./
RUN npm ci --omit=dev

COPY public ./public
COPY src ./src

ENV AERO_PROXY_HOST=0.0.0.0
ENV AERO_PROXY_PORT=8080

EXPOSE 8080

CMD ["npm", "start"]

