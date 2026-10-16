# Gallery viewer: Debian nginx + the image_filter module, so the 4K frames can
# be resized to small thumbnails on the fly (full 4K PNGs are 3–13 MB each,
# too heavy for a grid). Using the Debian nginx
# package together with its matching distro module avoids the "module is not
# binary compatible" error you hit loading a distro module into the official
# nginx.org image. worker_processes is capped so concurrent 4K decodes can't
# blow the container's RAM budget.
FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends nginx libnginx-mod-http-image-filter \
    && rm -f /etc/nginx/sites-enabled/default \
    && sed -i 's/worker_processes auto;/worker_processes 2;/' /etc/nginx/nginx.conf \
    && rm -rf /var/lib/apt/lists/*
EXPOSE 80
CMD ["nginx", "-g", "daemon off;"]
