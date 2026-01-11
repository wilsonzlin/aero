{{/*
Common template helpers for the aero-gateway chart.
*/}}

{{- define "aero-gateway.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "aero-gateway.fullname" -}}
{{- if .Values.fullnameOverride -}}
{{- .Values.fullnameOverride | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- $name := include "aero-gateway.name" . -}}
{{- if contains $name .Release.Name -}}
{{- .Release.Name | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- printf "%s-%s" .Release.Name $name | trunc 63 | trimSuffix "-" -}}
{{- end -}}
{{- end -}}
{{- end -}}

{{- define "aero-gateway.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" -}}
{{- end -}}

{{- define "aero-gateway.selectorLabels" -}}
app.kubernetes.io/name: {{ include "aero-gateway.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end -}}

{{- define "aero-gateway.labels" -}}
helm.sh/chart: {{ include "aero-gateway.chart" . }}
{{ include "aero-gateway.selectorLabels" . }}
{{- if .Chart.AppVersion }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
{{- end }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end -}}

{{- define "aero-gateway.configName" -}}
{{- if .Values.config.name -}}
{{- .Values.config.name -}}
{{- else -}}
{{- include "aero-gateway.fullname" . -}}
{{- end -}}
{{- end -}}

{{- define "aero-gateway.secretName" -}}
{{- if .Values.secrets.name -}}
{{- .Values.secrets.name -}}
{{- else -}}
{{- include "aero-gateway.fullname" . -}}
{{- end -}}
{{- end -}}

{{- define "aero-gateway.effectiveSecretName" -}}
{{- if .Values.secrets.create -}}
{{- include "aero-gateway.secretName" . -}}
{{- else -}}
{{- .Values.secrets.existingSecret -}}
{{- end -}}
{{- end -}}

{{- define "aero-gateway.redisFullname" -}}
{{- printf "%s-redis" (include "aero-gateway.fullname" .) -}}
{{- end -}}

{{- define "aero-gateway.publicBaseUrl" -}}
{{- if .Values.gateway.publicBaseUrl -}}
{{- .Values.gateway.publicBaseUrl -}}
{{- else -}}
{{- $scheme := "http" -}}
{{- if and .Values.ingress.enabled .Values.ingress.tls.enabled -}}
{{- $scheme = "https" -}}
{{- end -}}
{{- if and .Values.ingress.enabled .Values.ingress.host -}}
{{- printf "%s://%s" $scheme .Values.ingress.host -}}
{{- else -}}
{{- printf "%s://localhost:%v" $scheme .Values.gateway.containerPort -}}
{{- end -}}
{{- end -}}
{{- end -}}

{{- define "aero-gateway.tlsSecretName" -}}
{{- if .Values.ingress.tls.secretName -}}
{{- .Values.ingress.tls.secretName -}}
{{- else -}}
{{- printf "%s-tls" (include "aero-gateway.fullname" .) -}}
{{- end -}}
{{- end -}}

{{- define "aero-gateway.certManagerIssuerName" -}}
{{- if (default false .Values.certManager.createIssuer) -}}
{{- if .Values.certManager.issuer.name -}}
{{- .Values.certManager.issuer.name -}}
{{- else -}}
{{- include "aero-gateway.fullname" . -}}
{{- end -}}
{{- else -}}
{{- .Values.certManager.issuerRef.name -}}
{{- end -}}
{{- end -}}

{{- define "aero-gateway.certManagerIssuerKind" -}}
{{- if (default false .Values.certManager.createIssuer) -}}
Issuer
{{- else -}}
{{- .Values.certManager.issuerRef.kind -}}
{{- end -}}
{{- end -}}

{{- define "aero-gateway.certManagerIssuerAccountKeySecretName" -}}
{{- if .Values.certManager.issuer.privateKeySecretName -}}
{{- .Values.certManager.issuer.privateKeySecretName -}}
{{- else -}}
{{- printf "%s-account-key" (include "aero-gateway.certManagerIssuerName" .) -}}
{{- end -}}
{{- end -}}

{{- define "aero-gateway.coopCoepSnippet" -}}
add_header Cross-Origin-Opener-Policy "{{ .Values.ingress.coopCoep.coop }}" always;
add_header Cross-Origin-Embedder-Policy "{{ .Values.ingress.coopCoep.coep }}" always;
add_header Cross-Origin-Resource-Policy "{{ .Values.ingress.coopCoep.corp }}" always;
add_header Origin-Agent-Cluster "{{ .Values.ingress.coopCoep.originAgentCluster }}" always;
{{- end -}}

{{- define "aero-gateway.securityHeadersSnippet" -}}
add_header X-Content-Type-Options "{{ .Values.ingress.securityHeaders.xContentTypeOptions }}" always;
add_header Referrer-Policy "{{ .Values.ingress.securityHeaders.referrerPolicy }}" always;
add_header Permissions-Policy "{{ .Values.ingress.securityHeaders.permissionsPolicy }}" always;
add_header Content-Security-Policy "{{ .Values.ingress.securityHeaders.contentSecurityPolicy }}" always;
{{- end -}}

{{- define "aero-gateway.nginxIngressSnippet" -}}
{{- if .Values.ingress.coopCoep.enabled -}}
{{ include "aero-gateway.coopCoepSnippet" . }}
{{- end -}}
{{- if (default false .Values.ingress.securityHeaders.enabled) -}}
{{ include "aero-gateway.securityHeadersSnippet" . }}
{{- end -}}
{{- end -}}

{{- define "aero-gateway.securityHeadersSnippet" -}}
add_header X-Content-Type-Options "{{ .Values.ingress.securityHeaders.xContentTypeOptions }}" always;
add_header Referrer-Policy "{{ .Values.ingress.securityHeaders.referrerPolicy }}" always;
add_header Permissions-Policy "{{ .Values.ingress.securityHeaders.permissionsPolicy }}" always;
add_header Content-Security-Policy "{{ .Values.ingress.securityHeaders.contentSecurityPolicy }}" always;
{{- end -}}
