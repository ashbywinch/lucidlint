MODE = "prod"
TIMEOUT = 60


def connect():
    import socket
    return socket.socket()


def close(sock):
    return sock.close()
