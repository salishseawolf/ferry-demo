{{/* Every resource's name starts with the release's. */}}
{{- define "ferry.name" -}}
{{- .Release.Name | trunc 40 | trimSuffix "-" -}}
{{- end -}}

{{- define "ferry.labels" -}}
app.kubernetes.io/name: ferry-demo
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
helm.sh/chart: {{ printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" }}
{{- end -}}

{{/* Selector labels for one part: include "ferry.selector" (list . "server") */}}
{{- define "ferry.selector" -}}
app.kubernetes.io/name: ferry-demo
app.kubernetes.io/instance: {{ (index . 0).Release.Name }}
app.kubernetes.io/component: {{ index . 1 }}
{{- end -}}

{{- define "ferry.serverImage" -}}
{{ .Values.images.server.repository }}:{{ .Values.images.server.tag | default .Chart.AppVersion }}
{{- end -}}

{{- define "ferry.webImage" -}}
{{ .Values.images.web.repository }}:{{ .Values.images.web.tag | default .Chart.AppVersion }}
{{- end -}}

{{/* The Secret CloudNativePG makes for the database's owner. */}}
{{- define "ferry.dbSecret" -}}
{{ include "ferry.name" . }}-db-app
{{- end -}}

{{- define "ferry.databaseUrl" -}}
- name: DATABASE_URL
  valueFrom:
    secretKeyRef:
      name: {{ include "ferry.dbSecret" . }}
      key: uri
{{- end -}}

{{/* The server image's user, which owns nothing on disk. */}}
{{- define "ferry.serverPodSecurity" -}}
runAsNonRoot: true
runAsUser: 65532
runAsGroup: 65532
seccompProfile: { type: RuntimeDefault }
{{- end -}}

{{- define "ferry.containerSecurity" -}}
allowPrivilegeEscalation: false
readOnlyRootFilesystem: true
capabilities: { drop: [ALL] }
{{- end -}}

{{/* A job on the server image: include "ferry.serverJob" (list . "compute-advice" resources) */}}
{{- define "ferry.serverJob" -}}
{{- $ := index . 0 -}}
restartPolicy: OnFailure
securityContext:
  {{- include "ferry.serverPodSecurity" $ | nindent 2 }}
containers:
  - name: {{ index . 1 }}
    image: {{ include "ferry.serverImage" $ }}
    imagePullPolicy: {{ $.Values.images.pullPolicy }}
    command: ["ferry-server", {{ index . 1 | quote }}]
    env:
      {{- include "ferry.databaseUrl" $ | nindent 6 }}
      - name: RUST_LOG
        value: {{ $.Values.server.logLevel | quote }}
    resources:
      {{- toYaml (index . 2) | nindent 6 }}
    securityContext:
      {{- include "ferry.containerSecurity" $ | nindent 6 }}
{{- end -}}
