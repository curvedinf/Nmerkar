import sys
from math import sqrt


def multiply_av(vector, out):
    for i in range(len(vector)):
        denominator = i * (i + 3) // 2 + 1
        increment = i + 1
        total = 0.0
        for value in vector:
            total += value / denominator
            denominator += increment
            increment += 1
        out[i] = total


def multiply_atv(vector, out):
    for i in range(len(vector)):
        denominator = i * (i + 1) // 2 + 1
        increment = i + 2
        total = 0.0
        for value in vector:
            total += value / denominator
            denominator += increment
            increment += 1
        out[i] = total


def multiply_at_av(vector, tmp, out):
    multiply_av(vector, tmp)
    multiply_atv(tmp, out)


def main():
    n = int(sys.argv[1]) if len(sys.argv) > 1 else 5500
    u = [1.0] * n
    v = [0.0] * n
    tmp = [0.0] * n

    for _ in range(10):
        multiply_at_av(u, tmp, v)
        multiply_at_av(v, tmp, u)

    vbv = sum(a * b for a, b in zip(u, v))
    vv = sum(value * value for value in v)
    sys.stdout.write(f"{sqrt(vbv / vv):.9f}\n")


main()
