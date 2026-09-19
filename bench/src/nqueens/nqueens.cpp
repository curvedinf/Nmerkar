// nqueens.cpp — N-queens by iterative backtracking (bench/SPEC.md)
#include <cstdio>
#include <cstdlib>
int main(int argc, char** argv) {
  int n = argc > 1 ? atoi(argv[1]) : 11;
  int* cols = new int[n];
  int* nxt = new int[n + 1];
  long long cnt = 0;
  int d = 0;
  nxt[0] = 0;
  while (d >= 0) {
    int c = nxt[d];
    bool found = false;
    while (!found && c < n) {
      int i = 0;
      bool sf = true;
      while (i < d && sf) {
        int ci = cols[i];
        bool bad = ci == c || (ci - c) == (i - d) || (ci - c) == (d - i);
        sf = sf && !bad;
        i++;
      }
      if (sf) found = true;
      else c++;
    }
    if (c < n) {
      cols[d] = c;
      nxt[d] = c + 1;
      d++;
      nxt[d] = 0;
      if (d == n) {
        cnt++;
        d--;
      }
    } else {
      d--;
    }
  }
  printf("solutions: %lld\n", cnt);
  delete[] cols;
  delete[] nxt;
}
