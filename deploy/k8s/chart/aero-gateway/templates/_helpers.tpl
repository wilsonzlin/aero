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

{{- define "aero-gateway.coopCoepSnippet" -}}
add_header Cross-Origin-Opener-Policy "{{ .Values.ingress.coopCoep.coop }}" always;
add_header Cross-Origin-Embedder-Policy "{{ .Values.ingress.coopCoep.coep }}" always;
add_header Cross-Origin-Resource-Policy "{{ .Values.ingress.coopCoep.corp }}" always;
add_header Origin-Agent-Cluster "{{ .Values.ingress.coopCoep.originAgentCluster }}" always;
{{- end -}}
