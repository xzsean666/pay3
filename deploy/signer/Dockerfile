FROM python:3.11-alpine

WORKDIR /app

COPY reference_signer.py /app/reference_signer.py

RUN chmod +x /app/reference_signer.py

# Run as non-root user
RUN addgroup -S signer && adduser -S signer -G signer
USER signer

EXPOSE 8088

ENV SIGNER_HOST=0.0.0.0
ENV SIGNER_PORT=8088

HEALTHCHECK --interval=10s --timeout=3s --start-period=2s --retries=3 \
  CMD python3 -c "import urllib.request, os; sys_token=os.getenv('SIGNER_AUTH_TOKEN', ''); req=urllib.request.Request('http://127.0.0.1:8088/healthz'); req.add_header('Authorization', f'Bearer {sys_token}') if sys_token else None; res=urllib.request.urlopen(req); exit(0 if res.getcode()==200 else 1)"

ENTRYPOINT ["python3", "/app/reference_signer.py"]
