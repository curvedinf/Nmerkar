// bfs.cpp — breadth-first search over a deterministic graph (bench/SPEC.md)
#include <cstdio>
#include <cstdlib>
#include <vector>
int main(int argc, char** argv) {
  long n = argc > 1 ? atol(argv[1]) : 1000000;
  std::vector<std::vector<int>> adj(n);
  for (long u = 0; u < n; u++) {
    std::vector<int> lst;
    lst.push_back((int)((7 * u + 3) % n));
    lst.push_back((int)((13 * u + 11) % n));
    if (u % 3 == 0) lst.push_back((int)((u + 1) % n));
    adj[u] = lst;
  }
  std::vector<int> dist(n, -1);
  dist[0] = 0;
  std::vector<int> q(n + 1);
  q[0] = 0;
  long head = 0, tail = 1;
  while (head < tail) {
    int u = q[head++];
    int du = dist[u];
    for (int v : adj[u]) {
      if (dist[v] < 0) {
        dist[v] = du + 1;
        q[tail++] = v;
      }
    }
  }
  long r = 0, s = 0, m = 0;
  for (long i = 0; i < n; i++) {
    int d = dist[i];
    if (d >= 0) { r++; s += d; if (d > m) m = d; }
  }
  printf("reached: %ld\n", r);
  printf("unreached: %ld\n", n - r);
  printf("sum_dist: %ld\n", s);
  printf("max_dist: %ld\n", m);
}
