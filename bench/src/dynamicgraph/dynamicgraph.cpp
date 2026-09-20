// dynamicgraph.cpp — BFS over a hash map of dynamic neighbor lists.
#include <cstdio>
#include <cstdlib>
#include <unordered_map>
#include <utility>
#include <vector>

int main(int argc, char** argv) {
  long n = argc > 1 ? atol(argv[1]) : 1000000;
  std::unordered_map<int, std::vector<int>> adj;
  for (long u = 0; u < n; u++) {
    std::vector<int> neighbors;
    neighbors.push_back((int)((7 * u + 3) % n));
    neighbors.push_back((int)((13 * u + 11) % n));
    if (u % 3 == 0) neighbors.push_back((int)((u + 1) % n));
    adj.emplace((int)u, std::move(neighbors));
  }
  std::vector<int> dist(n, -1);
  std::vector<int> q(n);
  dist[0] = 0;
  q[0] = 0;
  long head = 0, tail = 1;
  while (head < tail) {
    int u = q[head++];
    int du = dist[u];
    const std::vector<int>& neighbors = adj.at(u);
    for (int v : neighbors) {
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
