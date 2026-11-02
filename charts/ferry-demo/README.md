# ferry-demo chart

Deploys the app: the server (one pod, which polls WSF and records everything), the web front end (nginx), a CloudNativePG Postgres, and three CronJobs: the daily advice job, a nightly `pg_dump`, and a rebuild that only runs when started by hand.

It needs these installed in the cluster: the CloudNativePG operator, the 1Password Operator, and Traefik 2.10 or later (for `traefik.io/v1alpha1`).

CI publishes the chart on every green push to `main`, as `oci://ghcr.io/salishseawolf/charts/ferry-demo`, version `0.1.<run number>`. Each version deploys the images built from the same commit (`sha-<commit>`), so a chart version pins the whole app. `mise run chart:lint` checks the chart locally.

## Values

`host` and `wsdot.itemPath` are required; [`values.yaml`](values.yaml) explains the rest, and [`ci/example-values.yaml`](ci/example-values.yaml) shows a cluster's. An Argo CD Application might look like:

```yaml
apiVersion: argoproj.io/v1alpha1
kind: Application
metadata:
  name: ferry-demo
  namespace: argocd
spec:
  project: default
  source:
    repoURL: ghcr.io/salishseawolf/charts
    chart: ferry-demo
    targetRevision: 0.1.42
    helm:
      valuesObject:
        host: ferries.example.org
        wsdot:
          itemPath: vaults/<vault>/items/<item>
  destination:
    server: https://kubernetes.default.svc
    namespace: ferry-demo
  syncPolicy:
    automated: { prune: true, selfHeal: true }
    syncOptions: [CreateNamespace=true]
```

The database Cluster and the backups' volume are marked so that neither an uninstall nor an Argo CD prune deletes them: deleting the Cluster would delete `raw_polls`, which nothing can bring back.

## Monitoring

The server's Service (port 8090) answers, besides the probes' `/healthz` and `/readyz`:

- `/recording`: 200 while boat positions were last recorded under 2 minutes ago, 503 with their age otherwise ("not recording: boat positions last 312 s ago"). It reads the database, so it fails when WSF stops answering usefully (an outage, a revoked key, an empty answer) and when answers can't be written. It isn't a probe: while it fails, the server still serves what it holds. nginx doesn't pass it on, so it isn't public.

## First deploy, starting from the dev database

So the record starts when dev's did, not on deployment day:

1. Deploy with `server.replicas: 0`, and wait for the database (`ferry-demo-db-1` running).
2. On the dev machine, with dev still running: `mise run dev:dump`.
3. Restore it, into the database the server hasn't touched yet:
   ```
   kubectl exec -i -n ferry-demo ferry-demo-db-1 -c postgres -- pg_restore -d ferry --no-owner --role=ferry < dumps/ferry-<time>.dump
   ```
4. Set `server.replicas: 1`. The server migrates the database if the chart is newer than the dump, and starts recording.
5. Once its `/readyz` answers and boats move on the map, stop the dev environment. Only the minutes between the dump and step 4 go unrecorded.

## Rebuilding history

After a fix to how history is derived from `raw_polls` (`ferry-server rebuild`). For now the server must be stopped while it runs, and nothing is recorded meanwhile:

1. Set `server.replicas: 0`, and wait for the server pod to go.
2. Start the rebuild and follow it:
   ```
   kubectl create job -n ferry-demo --from=cronjob/ferry-demo-rebuild ferry-demo-rebuild-$(date +%Y%m%d%H%M)
   kubectl logs -n ferry-demo -f job/ferry-demo-rebuild-<time>
   ```
3. Set `server.replicas: 1`.

## Backups

The `backup` CronJob writes a `pg_dump` every night to the `ferry-demo-backups` volume, keeping the newest 7 (`backup.keep`). An extra one, before a risky change:

```
kubectl create job -n ferry-demo --from=cronjob/ferry-demo-backup ferry-demo-backup-$(date +%Y%m%d%H%M)
```

A dump straight to this machine, for `DUMP=… mise run dev:offline`:

```
kubectl exec -n ferry-demo ferry-demo-db-1 -c postgres -- pg_dump -d ferry -Fc -Z 6 > dumps/ferry-prod-$(date -u +%Y%m%dT%H%MZ).dump
```

To restore one, stop the server first (`server.replicas: 0`), then `pg_restore --clean --if-exists -d ferry --no-owner --role=ferry` the same way as on the first deploy.
