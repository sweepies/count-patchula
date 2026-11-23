// The Count's own instance: `patchula serve` as one Deployment, applied by
// release.yml through `mise run instance:deploy`. The cluster provides the
// rest by name in the namespace (see internals/instance in the docs).
//
// No state is kept between runs: each run server-side applies the whole
// Deployment, which creates it or converges it.
import * as Kubernetes from "@alchemy.run/kubernetes";
import * as Alchemy from "alchemy";
import * as Config from "effect/Config";
import * as Effect from "effect/Effect";
import * as Redacted from "effect/Redacted";

const name = "count-patchula";
const instanceLabel = "count-patchula.sweepy.dev/instance";

export default Alchemy.Stack(
  "CountPatchulaInstance",
  {
    providers: Kubernetes.providers(),
    state: Alchemy.localState(),
  },
  Effect.gen(function* () {
    const endpoint = yield* Config.String("INSTANCE_ENDPOINT");
    const certificateAuthorityData = yield* Config.String("INSTANCE_CA");
    const namespace = yield* Config.String("INSTANCE_NAMESPACE").pipe(Config.withDefault(name));
    const token = yield* Config.Redacted("INSTANCE_TOKEN");
    const image = yield* Config.String("INSTANCE_IMAGE");
    const replicas = yield* Config.Int("INSTANCE_REPLICAS");

    const cluster: Kubernetes.Connection = {
      endpoint,
      certificateAuthorityData,
      auth: { kind: "token", token: Redacted.value(token) },
    };

    const labels = { "app.kubernetes.io/name": name };

    yield* Kubernetes.Manifest("Deployment", {
      cluster,
      manifest: {
        apiVersion: "apps/v1",
        kind: "Deployment",
        metadata: { name, namespace, labels },
        spec: {
          replicas,
          // One server per cache: never two pods on the volume.
          strategy: { type: "Recreate" },
          selector: { matchLabels: labels },
          template: {
            metadata: { labels },
            spec: {
              automountServiceAccountToken: false,
              // The node's own resolvers: the server talks only to GitHub and
              // registries, never to in-cluster Services, so it needs no
              // cluster DNS, which may run on another node.
              dnsPolicy: "Default",
              nodeSelector: { [instanceLabel]: "true" },
              tolerations: [{ key: instanceLabel, operator: "Exists", effect: "NoSchedule" }],
              // Longer than the server's 60-second drain.
              terminationGracePeriodSeconds: 75,
              securityContext: {
                runAsUser: 10001,
                runAsGroup: 10001,
                runAsNonRoot: true,
                fsGroup: 10001,
                fsGroupChangePolicy: "OnRootMismatch",
                seccompProfile: { type: "RuntimeDefault" },
              },
              containers: [
                {
                  name: "patchula",
                  image,
                  envFrom: [{ secretRef: { name } }],
                  ports: [{ name: "http", containerPort: 8080 }],
                  startupProbe: {
                    httpGet: { path: "/healthz", port: "http" },
                    periodSeconds: 2,
                    failureThreshold: 30,
                  },
                  livenessProbe: {
                    httpGet: { path: "/healthz", port: "http" },
                    periodSeconds: 30,
                    timeoutSeconds: 5,
                    failureThreshold: 3,
                  },
                  readinessProbe: {
                    httpGet: { path: "/readyz", port: "http" },
                    periodSeconds: 10,
                    timeoutSeconds: 5,
                  },
                  // Lockfile tools spike well above the server's own use.
                  resources: {
                    requests: { cpu: "500m", memory: "1Gi" },
                    limits: { memory: "4Gi" },
                  },
                  securityContext: {
                    allowPrivilegeEscalation: false,
                    readOnlyRootFilesystem: true,
                    capabilities: { drop: ["ALL"] },
                  },
                  volumeMounts: [
                    { name: "settings", mountPath: "/etc/patchula", readOnly: true },
                    { name: "data", mountPath: "/var/lib/patchula" },
                    { name: "tmp", mountPath: "/tmp" },
                  ],
                },
              ],
              volumes: [
                { name: "settings", configMap: { name: `${name}-settings` } },
                { name: "data", persistentVolumeClaim: { claimName: `${name}-data` } },
                { name: "tmp", emptyDir: {} },
              ],
            },
          },
        },
      },
    });
  }),
);
