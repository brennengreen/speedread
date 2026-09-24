package main

import "fmt"

// Server serves.
type Server struct {
	Port int
	Host string
}

type Handler interface {
	Serve(req string) error
	Close()
}

type ID = string

// Start starts the server.
func (s *Server) Start() error {
	fmt.Println("start", s.Port)
	s.Port++
	return nil
}

func NewServer(port int) *Server {
	s := &Server{Port: port}
	s.Host = "localhost"
	return s
}

func main() {
	s := NewServer(8080)
	_ = s.Start()
	fmt.Println("done")
}
