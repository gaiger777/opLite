# Orbivisor 웹 포털 컨테이너 예. 웹 UI 정적 파일 묶음을 nginx로 준다(API는 앞단이 노드로 보낸다).
#   packaging/portal/build-web.sh <version> packaging/portal
#   docker build --build-arg VERSION=<version> -t orbivisor-portal:<version> packaging/portal
# 컨테이너는 8080에서 듣고 root가 아닌 nginx 사용자로 돈다(nginx-unprivileged 이미지).
FROM nginxinc/nginx-unprivileged:1.27-alpine
ARG VERSION
# ADD는 tar.gz를 풀어 /tmp/orbivisor-web-<version>/에 둔다.
ADD orbivisor-web-${VERSION}.tar.gz /tmp/
USER root
RUN rm -rf /usr/share/nginx/html \
    && mv /tmp/orbivisor-web-${VERSION} /usr/share/nginx/html \
    && chmod -R a+rX /usr/share/nginx/html
USER nginx
COPY nginx.conf /etc/nginx/conf.d/default.conf
EXPOSE 8080
